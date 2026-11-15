use std::ffi::{CStr, c_char, c_void};
use std::io::{self, Write};
use std::ptr::null_mut;
use std::sync::{LazyLock, Mutex};

#[cfg_attr(target_vendor = "apple", link(name = "iconv"))]
unsafe extern "C" {
    fn iconv_open(to: *const c_char, from: *const c_char) -> *mut c_void;
    fn iconv_close(descriptor: *mut c_void) -> libc::c_int;
    fn iconv(
        descriptor: *mut c_void,
        input: *mut *mut c_char,
        input_left: *mut usize,
        output: *mut *mut c_char,
        output_left: *mut usize,
    ) -> usize;
}

enum Encoding {
    Utf8,
    Ascii,
    Other(Mutex<Converter>),
}

struct Converter(*mut c_void);

// SAFETY: the descriptor is accessed only through its owning mutex.
unsafe impl Send for Converter {}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: this instance owns the successfully opened descriptor.
        unsafe { iconv_close(self.0) };
    }
}

impl Encoding {
    fn resolve() -> Self {
        // SAFETY: locale queries return library-owned NUL-terminated strings;
        // iconv_open copies the encoding names before returning.
        unsafe {
            if libc::setlocale(libc::LC_CTYPE, c"".as_ptr()).is_null() {
                return Self::Ascii;
            }
            let codeset = libc::nl_langinfo(libc::CODESET);
            if codeset.is_null() {
                return Self::Ascii;
            }
            let name = CStr::from_ptr(codeset).to_bytes();
            if name.eq_ignore_ascii_case(b"UTF-8") || name.eq_ignore_ascii_case(b"UTF8") {
                return Self::Utf8;
            }
            if name.eq_ignore_ascii_case(b"ASCII")
                || name.eq_ignore_ascii_case(b"US-ASCII")
                || name.eq_ignore_ascii_case(b"ANSI_X3.4-1968")
            {
                return Self::Ascii;
            }
            let descriptor = iconv_open(codeset, c"UTF-8".as_ptr());
            if descriptor == (-1_isize) as *mut c_void {
                Self::Ascii
            } else {
                Self::Other(Mutex::new(Converter(descriptor)))
            }
        }
    }
}

impl Converter {
    fn convert(&mut self, input: &str, output: &mut [u8]) -> Option<usize> {
        let mut input_ptr = input.as_ptr().cast_mut().cast();
        let mut input_left = input.len();
        let mut output_ptr = output.as_mut_ptr().cast();
        let mut output_left = output.len();
        // SAFETY: all pointers and lengths describe the live input/output slices.
        // POSIX iconv does not modify input bytes. Reset before conversion and
        // flush afterward so each emitted character ends in the initial state.
        unsafe {
            iconv(self.0, null_mut(), null_mut(), null_mut(), null_mut());
            let result = iconv(
                self.0,
                &mut input_ptr,
                &mut input_left,
                &mut output_ptr,
                &mut output_left,
            );
            if result == usize::MAX || input_left != 0 {
                return None;
            }
            let length = output.len() - output_left;
            // Some iconv implementations replace unrepresentable characters.
            if result > 0 && length == 1 && matches!(output[0], b'?' | b'*') {
                return None;
            }
            if iconv(
                self.0,
                null_mut(),
                null_mut(),
                &mut output_ptr,
                &mut output_left,
            ) == usize::MAX
            {
                return None;
            }
        }
        Some(output.len() - output_left)
    }
}

pub(super) fn write(character: char, mut writer: impl Write) -> io::Result<()> {
    if character.is_ascii() {
        return writer.write_all(&[character as u8]);
    }
    static ENCODING: LazyLock<Encoding> = LazyLock::new(Encoding::resolve);
    let mut utf8 = [0; 4];
    let text = character.encode_utf8(&mut utf8);
    match &*ENCODING {
        Encoding::Utf8 => return writer.write_all(text.as_bytes()),
        Encoding::Ascii => {}
        Encoding::Other(converter) => {
            let mut output = [0; 64];
            let mut converter = converter.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(length) = converter.convert(text, &mut output) {
                return writer.write_all(&output[..length]);
            }
        }
    }
    let code = character as u32;
    if code < 0x10000 {
        write!(writer, "\\u{code:04X}")
    } else {
        write!(writer, "\\U{code:08X}")
    }
}
