// This file is part of AXE's patches to uutils; see the package LICENSE.

use std::ffi::{CString, c_char, c_int, c_void};
use std::io::{self, Write};
use std::ops::ControlFlow;
use uucore::display::Quotable;
use uucore::error::set_exit_code;
use uucore::format::{FormatArguments, FormatChar, FormatError, FormatItem, Spec};
use uucore::{os_str_as_bytes, show_error, show_warning};

#[repr(C)]
struct NativeOutput {
    data: *mut u8,
    length: usize,
    converted: usize,
    ignored: usize,
    issue: c_int,
}

impl Default for NativeOutput {
    fn default() -> Self {
        Self {
            data: std::ptr::null_mut(),
            length: 0,
            converted: 0,
            ignored: 0,
            issue: 0,
        }
    }
}

unsafe extern "C" {
    fn axe_printf_long_double(
        operand: *const c_char,
        supplied: c_int,
        format: *const c_char,
        width: c_int,
        precision: c_int,
        scratch: *mut u8,
        capacity: usize,
        output: *mut NativeOutput,
    ) -> c_int;
    fn axe_printf_free(data: *mut c_void);
}

struct Allocation(*mut u8);
impl Drop for Allocation {
    fn drop(&mut self) {
        // SAFETY: this pointer is either null or the bridge's malloc allocation.
        unsafe { axe_printf_free(self.0.cast()) };
    }
}

#[derive(Clone, Copy)]
enum Parameter<'a> {
    Absent,
    Fixed(&'a [u8]),
    Dynamic(Option<&'a [u8]>),
}

impl Parameter<'_> {
    fn resolve(self, args: &mut FormatArguments, precision: bool) -> Result<i32, FormatError> {
        let value = match self {
            Self::Absent => return Ok(if precision { -1 } else { 0 }),
            Self::Fixed([]) => 0,
            Self::Fixed(digits) => std::str::from_utf8(digits)
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(size_error)?,
            Self::Dynamic(position) => {
                // uucore keeps the argument-location type private. Parsing this
                // integer spec uses its own cursor and diagnostic semantics.
                let prefix = position.unwrap_or_default();
                let mut body = [0; 32];
                if prefix.len() >= body.len() {
                    return Err(size_error());
                }
                body[..prefix.len()].copy_from_slice(prefix);
                body[prefix.len()] = b'd';
                let spec = Spec::parse(&mut &body[..prefix.len() + 1]).map_err(|_| size_error())?;
                let Spec::SignedInt { position, .. } = spec else {
                    unreachable!()
                };
                args.next_i64(position)
            }
        };
        if precision && value < 0 {
            return Ok(-1);
        }
        let value = i32::try_from(value).map_err(|_| size_error())?;
        if value == i32::MIN {
            return Err(size_error());
        }
        Ok(value)
    }
}

fn size_error() -> FormatError {
    FormatError::IoError(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid field width or precision",
    ))
}

struct Directive<'a> {
    position: Option<&'a [u8]>,
    flags: &'a [u8],
    width: Parameter<'a>,
    precision: Parameter<'a>,
    conversion: u8,
}

fn position<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let end = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if end != 0 && rest.get(end) == Some(&b'$') {
        let result = &rest[..=end];
        *rest = &rest[end + 1..];
        Some(result)
    } else {
        None
    }
}

fn parameter<'a>(rest: &mut &'a [u8]) -> Parameter<'a> {
    if rest.first() == Some(&b'*') {
        *rest = &rest[1..];
        Parameter::Dynamic(position(rest))
    } else {
        let end = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let digits = &rest[..end];
        *rest = &rest[end..];
        Parameter::Fixed(digits)
    }
}

impl<'a> Directive<'a> {
    // Only called for a spec already accepted by uucore's format parser.
    fn parse(raw: &'a [u8]) -> Self {
        let mut rest = &raw[1..];
        let position = position(&mut rest);
        let end = rest.iter().take_while(|b| b"-+ #0'".contains(b)).count();
        let flags = &rest[..end];
        rest = &rest[end..];
        let width = parameter(&mut rest);
        let precision = if rest.first() == Some(&b'.') {
            rest = &rest[1..];
            parameter(&mut rest)
        } else {
            Parameter::Absent
        };
        Self {
            position,
            flags,
            width,
            precision,
            conversion: *raw.last().unwrap(),
        }
    }
}

// Keep a presence cursor alongside uucore's cursor: a missing numeric operand
// is zero, whereas an explicitly empty operand is a conversion error.
pub(super) struct Writer<'a> {
    format: &'a [u8],
    remaining: &'a [u8],
    argument_count: usize,
    next: usize,
    highest: Option<usize>,
    offset: usize,
}

impl<'a> Writer<'a> {
    pub(super) fn new(format: &'a [u8], argument_count: usize) -> Self {
        Self {
            format,
            remaining: format,
            argument_count,
            next: 0,
            highest: None,
            offset: 0,
        }
    }

    pub(super) fn next_batch(&mut self) {
        self.offset = self
            .next
            .max(self.highest.map_or(0, |n| n.saturating_add(1)));
        self.next = self.offset;
        self.remaining = self.format;
    }

    fn argument(&mut self, position: Option<&[u8]>) -> bool {
        let index = if let Some(position) = position {
            let index = position[..position.len() - 1]
                .iter()
                .fold(0usize, |value, digit| {
                    value
                        .saturating_mul(10)
                        .saturating_add(usize::from(digit - b'0'))
                })
                .saturating_sub(1)
                .saturating_add(self.offset);
            self.highest = Some(self.highest.map_or(index, |n| n.max(index)));
            index
        } else {
            let index = self.next;
            self.next = self.next.saturating_add(1);
            index
        };
        index < self.argument_count
    }

    fn raw_spec(&mut self) -> &'a [u8] {
        while let Some(&first) = self.remaining.first() {
            match first {
                b'\\' => self.remaining = &self.remaining[2.min(self.remaining.len())..],
                b'%' if self.remaining.get(1) == Some(&b'%') => {
                    self.remaining = &self.remaining[2..]
                }
                b'%' => {
                    let end = self.remaining[1..]
                        .iter()
                        .position(|b| b"csbqdiuoxXfFeEgGaA".contains(b))
                        .unwrap()
                        + 2;
                    let raw = &self.remaining[..end];
                    self.remaining = &self.remaining[end..];
                    return raw;
                }
                _ => self.remaining = &self.remaining[1..],
            }
        }
        unreachable!("every parsed spec has a source directive")
    }

    pub(super) fn write<C: FormatChar>(
        &mut self,
        item: FormatItem<C>,
        mut output: impl Write,
        args: &mut FormatArguments,
    ) -> Result<ControlFlow<()>, FormatError> {
        let FormatItem::Spec(spec) = &item else {
            return item.write(output, args);
        };
        let raw = self.raw_spec();
        let directive = Directive::parse(raw);
        if let Parameter::Dynamic(position) = directive.width {
            self.argument(position);
        }
        if let Parameter::Dynamic(position) = directive.precision {
            self.argument(position);
        }
        let supplied = self.argument(directive.position);
        let Spec::Float { position, .. } = spec else {
            return item.write(output, args);
        };
        let width = directive.width.resolve(args, false)?;
        let precision = directive.precision.resolve(args, true)?;
        let operand = args.next_string(*position);
        let bytes = os_str_as_bytes(operand)?;
        let operand_c = CString::new(bytes).map_err(|_| size_error())?;
        let mut format = Vec::with_capacity(directive.flags.len() + 8);
        format.push(b'%');
        format.extend_from_slice(directive.flags);
        format.extend_from_slice(b"*.*L");
        format.push(directive.conversion);
        let format = CString::new(format).map_err(|_| size_error())?;
        let mut scratch = [0u8; 256];
        let mut result = NativeOutput::default();
        // SAFETY: both strings are NUL-terminated; format contains only validated
        // flags plus exactly two int stars and one long-double conversion. The
        // scratch/result buffers remain live and exclusively borrowed by C.
        let error = unsafe {
            axe_printf_long_double(
                operand_c.as_ptr(),
                i32::from(supplied),
                format.as_ptr(),
                width,
                precision,
                scratch.as_mut_ptr(),
                scratch.len(),
                &mut result,
            )
        };
        if error != 0 {
            return Err(FormatError::IoError(io::Error::from_raw_os_error(error)));
        }
        let _allocation = Allocation(if result.data == scratch.as_mut_ptr() {
            std::ptr::null_mut()
        } else {
            result.data
        });
        match result.issue {
            0 => {}
            issue => {
                set_exit_code(1);
                let reason = match issue {
                    1 => "expected a numeric value",
                    2 => "Numerical result out of range",
                    _ => "value not completely converted",
                };
                show_error!("{}: {reason}", operand.quote());
            }
        }
        if result.ignored != 0 && std::env::var_os("POSIXLY_CORRECT").is_none() {
            show_warning!(
                "{}: character(s) following character constant have been ignored",
                String::from_utf8_lossy(&bytes[result.ignored..])
            );
        }
        // SAFETY: success returns either scratch or a malloc buffer with exactly
        // length initialized output bytes; Allocation frees only the latter.
        let bytes = unsafe { std::slice::from_raw_parts(result.data, result.length) };
        output.write_all(bytes).map_err(FormatError::IoError)?;
        Ok(ControlFlow::Continue(()))
    }
}
