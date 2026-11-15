use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;

use bstr::{BString, ByteSlice, ByteVec};

use crate::ast::Expr;
use crate::error::{Error, Result, SourceLocation};
use crate::value::Value;

use super::{Interpreter, input::whitespace_fields};
use crate::regex::Regex;

impl<'a> Interpreter<'a> {
    /// Call a function with special handling for builtins that need AST access
    pub fn call_function<W: Write>(
        &mut self,
        name: &str,
        args: &[Expr],
        location: SourceLocation,
        output: &mut W,
    ) -> Result<Value> {
        // Check for built-in functions that need special argument handling
        match name {
            "sub" | "gsub" => return self.call_regex_sub(name, args, location),
            "gensub" => return self.call_gensub(args),
            "match" => return self.call_match(args, location),
            "split" => return self.call_split(args, location),
            "patsplit" => return self.call_patsplit(args, location),
            "asort" | "asorti" => return self.call_asort(name == "asorti", args, location),
            "close" => return self.call_close(args, location),
            "fflush" => return self.call_fflush(args, location, output),
            "system" => return self.call_system(args, output),
            "length" => {
                // gawk extension: length(array) returns element count.
                // Only intercept when the sole argument names a known array;
                // otherwise fall through to the normal scalar handling below.
                if let Some(Expr::Var(var_name, _)) = args.first() {
                    // Resolve pass-by-reference aliases so length(a) works on
                    // arrays received as function parameters.
                    if let Some(arr) = self.arrays.get(self.resolve_array_name(var_name)) {
                        return Ok(Value::Number(arr.len() as f64));
                    }
                }
            }
            _ => {}
        }

        // Evaluate all arguments for other functions
        let arg_values: Result<Vec<Value>> = args.iter().map(|e| self.eval_expr(e)).collect();
        let arg_values = arg_values?;

        // Check for other built-in functions
        if let Some(result) = self.call_builtin(name, &arg_values)? {
            return Ok(result);
        }

        // Check for user-defined functions
        if let Some(func) = self.functions.get(name).cloned() {
            // Extract array names from arguments for pass-by-reference
            let array_refs: Vec<Option<String>> = args
                .iter()
                .map(|e| match e {
                    Expr::Var(name, _) if self.arrays.contains_key(name) => Some(name.clone()),
                    _ => None,
                })
                .collect();
            return self.call_user_function(func, arg_values, array_refs, output);
        }

        Err(Error::runtime_at(
            format!("undefined function: {}", name),
            location.line,
            location.column,
        ))
    }

    /// Extract regex pattern from an expression (handles both regex literals and strings)
    fn extract_pattern(&mut self, expr: &Expr) -> Result<BString> {
        match expr {
            Expr::Regex(pattern, _) => Ok(pattern.as_bytes().into()),
            other => Ok(self.eval_expr(other)?.to_string_val()),
        }
    }

    /// Call sub or gsub with proper regex and target handling
    fn call_regex_sub(
        &mut self,
        name: &str,
        args: &[Expr],
        _location: SourceLocation,
    ) -> Result<Value> {
        let global = name == "gsub";

        let pattern = args
            .first()
            .map(|e| self.extract_pattern(e))
            .transpose()?
            .unwrap_or_default();

        let replacement = args
            .get(1)
            .map(|e| self.eval_expr(e))
            .transpose()?
            .map(|v| v.to_string_val())
            .unwrap_or_default();

        // Get the target (third argument or $0)
        let (target_value, target_expr) = if let Some(target_arg) = args.get(2) {
            (
                self.eval_expr(target_arg)?.to_string_val(),
                Some(target_arg),
            )
        } else {
            (self.record.clone(), None)
        };

        let re = self.get_regex(&pattern)?;
        let (new_str, count) = substitute(
            &re,
            &replacement,
            &target_value,
            false,
            if global { None } else { Some(1) },
        )?;

        // Assign the result back to the target
        if let Some(target_arg) = target_expr {
            self.assign_to_lvalue(target_arg, Value::from_string(new_str))?;
        } else {
            self.set_record(new_str)?;
        }

        Ok(Value::Number(count as f64))
    }

    /// Call match with proper regex handling
    fn call_match(&mut self, args: &[Expr], _location: SourceLocation) -> Result<Value> {
        let s = args
            .first()
            .map(|e| self.eval_expr(e))
            .transpose()?
            .map(|v| v.to_string_val())
            .unwrap_or_default();

        let pattern = args
            .get(1)
            .map(|e| self.extract_pattern(e))
            .transpose()?
            .unwrap_or_default();

        let re = self.get_regex(&pattern)?;

        if let Some(found) = re.find(&s, 0)? {
            self.rstart = crate::value::character_count(&s[..found.start], self.multibyte) + 1;
            self.rlength = crate::value::character_count(&s[found], self.multibyte) as i32;
            Ok(Value::Number(self.rstart as f64))
        } else {
            self.rstart = 0;
            self.rlength = -1;
            Ok(Value::Number(0.0))
        }
    }

    fn call_gensub(&mut self, args: &[Expr]) -> Result<Value> {
        let pattern = args
            .first()
            .map(|expr| self.extract_pattern(expr))
            .transpose()?
            .unwrap_or_default();
        let replacement = args
            .get(1)
            .map(|expr| self.eval_expr(expr))
            .transpose()?
            .map(|value| value.to_string_val())
            .unwrap_or_default();
        let how = args
            .get(2)
            .map(|expr| self.eval_expr(expr))
            .transpose()?
            .map(|value| value.to_string_val())
            .unwrap_or_else(|| "g".into());
        let target = args
            .get(3)
            .map(|expr| self.eval_expr(expr))
            .transpose()?
            .map(|value| value.to_string_val())
            .unwrap_or_else(|| self.record.clone());
        let occurrence = if how.eq_ignore_ascii_case(b"g") {
            None
        } else {
            Some(crate::value::parse_leading_number(&how).max(1.0) as usize)
        };
        let re = self.get_regex(&pattern)?;
        let (result, _) = substitute(&re, &replacement, &target, true, occurrence)?;
        Ok(Value::from_string(result))
    }

    /// Call split with proper array name handling
    fn call_split(&mut self, args: &[Expr], location: SourceLocation) -> Result<Value> {
        let s = args
            .first()
            .map(|e| self.eval_expr(e))
            .transpose()?
            .map(|v| v.to_string_val())
            .unwrap_or_default();

        // Get array name from second argument (must be a variable name)
        let array_name = match args.get(1) {
            Some(Expr::Var(name, _)) => name.clone(),
            Some(Expr::ArrayAccess { array, .. }) => array.clone(),
            Some(_) => {
                return Err(Error::runtime_at(
                    "split: second argument must be an array",
                    location.line,
                    location.column,
                ));
            }
            None => {
                return Err(Error::runtime_at(
                    "split: missing array argument",
                    location.line,
                    location.column,
                ));
            }
        };

        // Get separator (third argument or default FS)
        let sep = if let Some(sep_expr) = args.get(2) {
            self.extract_pattern(sep_expr)?
        } else {
            self.fs.clone()
        };

        // Clear the array
        self.arrays.remove(&array_name);

        // Split and populate array
        let parts: Vec<&[u8]> = if s.is_empty() {
            Vec::new()
        } else if sep.is_empty() {
            let mut parts = Vec::new();
            let mut at = 0;
            while at < s.len() {
                let end = crate::value::character_end(&s, at, self.multibyte);
                parts.push(&s[at..end]);
                at = end;
            }
            parts
        } else if sep == " " {
            whitespace_fields(&s).collect()
        } else if sep.len() == 1 {
            s.split(|byte| *byte == sep[0]).collect()
        } else {
            self.get_regex(&sep)?.split(&s)?
        };

        for (i, part) in parts.iter().enumerate() {
            let key = (i + 1).to_string();
            self.set_array_element(&array_name, key.as_bytes(), Value::from_string(*part));
        }

        Ok(Value::Number(parts.len() as f64))
    }

    /// asort(source [, dest]) - sort array values
    /// asorti(source [, dest]) - sort array indices
    fn call_asort(
        &mut self,
        sort_indices: bool,
        args: &[Expr],
        location: SourceLocation,
    ) -> Result<Value> {
        // Get source array name
        let source_name = match args.first() {
            Some(Expr::Var(name, _)) => name.clone(),
            _ => {
                return Err(Error::runtime_at(
                    if sort_indices {
                        "asorti: first argument must be an array"
                    } else {
                        "asort: first argument must be an array"
                    },
                    location.line,
                    location.column,
                ));
            }
        };

        // Get optional destination array name
        let dest_name = match args.get(1) {
            Some(Expr::Var(name, _)) => Some(name.clone()),
            None => None,
            _ => {
                return Err(Error::runtime_at(
                    if sort_indices {
                        "asorti: second argument must be an array"
                    } else {
                        "asort: second argument must be an array"
                    },
                    location.line,
                    location.column,
                ));
            }
        };

        // Get values to sort
        let items: Vec<BString> = if let Some(arr) = self.arrays.get(&source_name) {
            if sort_indices {
                arr.keys().cloned().collect()
            } else {
                arr.values().map(|v| v.to_string_val()).collect()
            }
        } else {
            Vec::new()
        };

        let mut sorted = items;
        sorted.sort();

        let count = sorted.len();

        // Store in destination (or source if no dest)
        let target = dest_name.unwrap_or_else(|| source_name.clone());
        self.arrays.remove(&target);

        for (i, item) in sorted.iter().enumerate() {
            let key = (i + 1).to_string();
            self.set_array_element(&target, key.as_bytes(), Value::from_string(item.clone()));
        }

        Ok(Value::Number(count as f64))
    }

    /// patsplit(string, array, fieldpat [, seps]) - split by pattern matches
    fn call_patsplit(&mut self, args: &[Expr], location: SourceLocation) -> Result<Value> {
        // Get string to split
        let s = args
            .first()
            .map(|e| self.eval_expr(e))
            .transpose()?
            .map(|v| v.to_string_val())
            .unwrap_or_default();

        // Get array name
        let array_name = match args.get(1) {
            Some(Expr::Var(name, _)) => name.clone(),
            _ => {
                return Err(Error::runtime_at(
                    "patsplit: second argument must be an array",
                    location.line,
                    location.column,
                ));
            }
        };

        // Get field pattern
        let fieldpat = if let Some(pat_expr) = args.get(2) {
            self.extract_pattern(pat_expr)?
        } else {
            return Err(Error::runtime_at(
                "patsplit: missing fieldpat argument",
                location.line,
                location.column,
            ));
        };

        // Optional separator array
        let seps_name = match args.get(3) {
            Some(Expr::Var(name, _)) => Some(name.clone()),
            None => None,
            _ => None,
        };

        // Clear destination arrays
        self.arrays.remove(&array_name);
        if let Some(ref name) = seps_name {
            self.arrays.remove(name);
        }

        // Compile regex and find all matches
        let re = self.get_regex(&fieldpat)?;
        let matches = re.ranges(&s)?;

        // Store matches in array
        for (i, mat) in matches.iter().enumerate() {
            let key = (i + 1).to_string();
            self.set_array_element(
                &array_name,
                key.as_bytes(),
                Value::from_string(&s[mat.clone()]),
            );
        }

        // Store separators if requested
        if let Some(ref name) = seps_name {
            let mut last_end = 0;
            for (i, mat) in matches.iter().enumerate() {
                let sep = &s[last_end..mat.start];
                let key = i.to_string();
                self.set_array_element(name, key.as_bytes(), Value::from_string(sep));
                last_end = mat.end;
            }
            // Final separator after last match
            let key = matches.len().to_string();
            self.set_array_element(name, key.as_bytes(), Value::from_string(&s[last_end..]));
        }

        Ok(Value::Number(matches.len() as f64))
    }

    /// Call close to close a file or pipe
    fn call_close(&mut self, args: &[Expr], location: SourceLocation) -> Result<Value> {
        let filename = args
            .first()
            .map(|e| self.eval_expr(e))
            .transpose()?
            .map(|v| v.to_string_val())
            .unwrap_or_default();

        // Remove from any of our file/pipe collections
        let found = self.output_files.remove(&filename).is_some()
            || self.input_files.remove(&filename).is_some()
            || self.pipes.remove(&filename).is_some();

        let _ = location;
        Ok(Value::Number(if found { 0.0 } else { -1.0 }))
    }

    /// Call fflush to flush output
    fn call_fflush<W: Write>(
        &mut self,
        args: &[Expr],
        _location: SourceLocation,
        output: &mut W,
    ) -> Result<Value> {
        if args.is_empty() {
            // Flush all output
            output.flush().map_err(Error::Io)?;
            for file in self.output_files.values_mut() {
                let _ = file.flush();
            }
            Ok(Value::Number(0.0))
        } else {
            let filename = self.eval_expr(&args[0])?.to_string_val();
            if filename.is_empty() {
                output.flush().map_err(Error::Io)?;
                Ok(Value::Number(0.0))
            } else if let Some(file) = self.output_files.get_mut(&filename) {
                file.flush().map_err(Error::Io)?;
                Ok(Value::Number(0.0))
            } else {
                Ok(Value::Number(-1.0))
            }
        }
    }

    /// `system(cmd)`: POSIX requires all buffered output to be flushed before the
    /// command runs, so its output interleaves with ours in the expected order.
    fn call_system<W: Write>(&mut self, args: &[Expr], output: &mut W) -> Result<Value> {
        let cmd = match args.first() {
            Some(expr) => self.eval_expr(expr)?.to_string_val(),
            None => BString::default(),
        };

        output.flush().map_err(Error::Io)?;
        for file in self.output_files.values_mut() {
            let _ = file.flush();
        }

        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(OsStr::from_bytes(&cmd))
            .status()
            .map(|s| s.code().unwrap_or(-1))
            .unwrap_or(-1);
        Ok(Value::Number(status as f64))
    }

    fn call_builtin(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>> {
        match name {
            // String functions
            "length" => {
                let s = args
                    .first()
                    .map(|v| v.to_string_val())
                    .unwrap_or_else(|| self.record.clone());
                Ok(Some(Value::Number(
                    crate::value::character_count(&s, self.multibyte) as f64,
                )))
            }

            "substr" => {
                let s = args.first().map(|v| v.to_string_val()).unwrap_or_default();
                let start = args.get(1).map(|v| v.to_number() as usize).unwrap_or(1);
                let len = args.get(2).map(|v| v.to_number() as usize);

                // AWK uses 1-based indexing; ensure start is at least 1
                let start = start.max(1).saturating_sub(1);
                let start = crate::value::byte_offset(&s, start, self.multibyte);
                let end = len
                    .map(|length| {
                        start + crate::value::byte_offset(&s[start..], length, self.multibyte)
                    })
                    .unwrap_or(s.len());
                let result = &s[start..end];
                Ok(Some(Value::from_string(result)))
            }

            "index" => {
                let s = args.first().map(|v| v.to_string_val()).unwrap_or_default();
                let target = args.get(1).map(|v| v.to_string_val()).unwrap_or_default();
                let pos = s
                    .find(&target)
                    .map(|index| crate::value::character_count(&s[..index], self.multibyte) + 1)
                    .unwrap_or(0);
                Ok(Some(Value::Number(pos as f64)))
            }

            "sprintf" => {
                let format = args.first().map(|v| v.to_string_val()).unwrap_or_default();
                let rest = if args.len() > 1 { &args[1..] } else { &[] };
                let result = self.format_printf(&format, rest);
                Ok(Some(Value::from_string(result)))
            }

            "tolower" => {
                let s = args.first().map(|v| v.to_string_val()).unwrap_or_default();
                Ok(Some(Value::from_string(crate::value::change_case(
                    &s,
                    false,
                    self.multibyte,
                ))))
            }

            "toupper" => {
                let s = args.first().map(|v| v.to_string_val()).unwrap_or_default();
                Ok(Some(Value::from_string(crate::value::change_case(
                    &s,
                    true,
                    self.multibyte,
                ))))
            }

            "strtonum" => {
                let number = match args.first() {
                    Some(Value::Number(number) | Value::NumericString(_, number)) => *number,
                    Some(value) => strtonum(&value.as_bytes()),
                    None => 0.0,
                };
                Ok(Some(Value::Number(number)))
            }

            // Math functions
            "sin" => {
                let n = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                Ok(Some(Value::Number(n.sin())))
            }

            "cos" => {
                let n = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                Ok(Some(Value::Number(n.cos())))
            }

            "atan2" => {
                let y = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                let x = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                Ok(Some(Value::Number(y.atan2(x))))
            }

            "exp" => {
                let n = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                Ok(Some(Value::Number(n.exp())))
            }

            "log" => {
                let n = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                Ok(Some(Value::Number(n.ln())))
            }

            "sqrt" => {
                let n = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                Ok(Some(Value::Number(n.sqrt())))
            }

            "int" => {
                let n = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                Ok(Some(Value::Number(n.trunc())))
            }

            "rand" => {
                // Use the internal RNG state
                let random = self.next_random();
                Ok(Some(Value::Number(random)))
            }

            "srand" => {
                let old_seed = self.rand_seed;
                if let Some(seed) = args.first() {
                    self.rand_seed = seed.to_number() as u64;
                } else {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    self.rand_seed = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                }
                self.rand_state = self.rand_seed;
                Ok(Some(Value::Number(old_seed as f64)))
            }

            // System functions
            // === GAWK Extensions ===

            // Time functions
            "systime" => {
                // Return current time as seconds since epoch
                use std::time::{SystemTime, UNIX_EPOCH};
                let secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Ok(Some(Value::Number(secs as f64)))
            }

            "mktime" => {
                // Parse "YYYY MM DD HH MM SS [DST]" into epoch timestamp
                let datespec = args.first().map(|v| v.to_string_val()).unwrap_or_default();
                let parts: Vec<i64> = whitespace_fields(&datespec)
                    .filter_map(|field| std::str::from_utf8(field).ok()?.parse().ok())
                    .collect();

                if parts.len() >= 6 {
                    // Simple implementation using chrono-like calculation
                    // This is a simplified version; for full accuracy we'd need chrono crate
                    let year = parts[0];
                    let month = parts[1];
                    let day = parts[2];
                    let hour = parts[3];
                    let min = parts[4];
                    let sec = parts[5];

                    // Simplified epoch calculation (not handling DST or timezones)
                    let epoch = simple_mktime(year, month, day, hour, min, sec);
                    Ok(Some(Value::Number(epoch as f64)))
                } else {
                    Ok(Some(Value::Number(-1.0)))
                }
            }

            "strftime" => {
                // Format timestamp
                let format = args
                    .first()
                    .map(|v| v.to_string_val())
                    .unwrap_or_else(|| "%a %b %e %H:%M:%S %Z %Y".into());
                use std::time::{SystemTime, UNIX_EPOCH};
                let timestamp = args
                    .get(1)
                    .map(|v| v.to_number() as u64)
                    .unwrap_or_else(|| {
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    });

                let result = format_strftime(&format, timestamp);
                Ok(Some(Value::from_string(result)))
            }

            _ => Ok(None), // Not a built-in
        }
    }

    fn call_user_function<W: Write>(
        &mut self,
        func: &crate::ast::FunctionDef,
        args: Vec<Value>,
        array_refs: Vec<Option<String>>,
        output: &mut W,
    ) -> Result<Value> {
        // Save current variables for local scope
        let saved_vars: std::collections::HashMap<String, Value> = func
            .params
            .iter()
            .filter_map(|name| self.variables.get(name).map(|v| (name.clone(), v.clone())))
            .collect();

        // Save any arrays that share names with parameters (for local arrays)
        let saved_arrays: std::collections::HashMap<
            String,
            std::collections::HashMap<BString, Value>,
        > = func
            .params
            .iter()
            .filter_map(|name| self.arrays.get(name).map(|a| (name.clone(), a.clone())))
            .collect();

        // Create array aliases for pass-by-reference
        // If an argument is an array reference, the parameter name should point to the same array
        let mut array_aliases: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if let Some(Some(array_name)) = array_refs.get(i) {
                // This parameter is an array reference
                // Create an alias: when we access param, we should access array_name
                if param != array_name {
                    array_aliases.insert(param.clone(), array_name.clone());
                }
            }
        }
        let saved_aliases = std::mem::replace(&mut self.array_aliases, array_aliases);

        // Set scalar parameters
        for (i, param) in func.params.iter().enumerate() {
            // Skip if this is an array reference
            if let Some(Some(_)) = array_refs.get(i) {
                continue;
            }
            let value = args.get(i).cloned().unwrap_or(Value::Uninitialized);
            self.set_variable_value(param, value)?;
        }

        // Execute function body, passing the actual output
        let result = self.execute_block(&func.body, output);

        self.array_aliases = saved_aliases;

        // Restore saved variables and remove parameters that weren't saved
        for param in &func.params {
            if let Some(value) = saved_vars.get(param) {
                self.set_variable_value(param, value.clone())?;
            } else {
                self.variables.remove(param);
            }
        }

        // Restore any saved arrays or remove local arrays
        for param in &func.params {
            if let Some(arr) = saved_arrays.get(param) {
                self.arrays.insert(param.clone(), arr.clone());
            } else if !array_refs
                .get(
                    func.params
                        .iter()
                        .position(|p| p == param)
                        .unwrap_or(usize::MAX),
                )
                .map(|r| r.is_some())
                .unwrap_or(false)
            {
                self.arrays.remove(param);
            }
        }

        match result? {
            super::stmt::StmtResult::Return(value) => Ok(value),
            super::stmt::StmtResult::Control => Err(Error::ControlFlow),
            _ => Ok(Value::Uninitialized),
        }
    }

    /// Generate a random number between 0 and 1 using xorshift64
    fn next_random(&mut self) -> f64 {
        let mut x = self.rand_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rand_state = x;
        (x as f64) / (u64::MAX as f64)
    }
}

fn substitute(
    regex: &Regex,
    replacement: &[u8],
    target: &[u8],
    allow_groups: bool,
    occurrence: Option<usize>,
) -> Result<(BString, usize)> {
    let mut result = Vec::with_capacity(target.len());
    let mut from = 0;
    let mut copied = 0;
    let mut matches = 0;
    let mut replacements = 0;
    let mut previous_end = None;
    let matcher = regex.matcher(target);
    while let Some(captures) = matcher.captures(from)? {
        let (start, end) = captures.pos(0).expect("capture zero is the whole match");
        if start == end && previous_end == Some(start) {
            from = matcher.advance(end);
            continue;
        }
        matches += 1;
        if occurrence.is_none_or(|number| matches == number) {
            result.extend_from_slice(&target[copied..start]);
            expand_replacement(&mut result, replacement, target, &captures, allow_groups);
            copied = end;
            replacements += 1;
            if occurrence.is_some() {
                break;
            }
        }
        previous_end = Some(end);
        from = if start == end {
            matcher.advance(end)
        } else {
            end
        };
    }
    result.extend_from_slice(&target[copied..]);
    Ok((result.into(), replacements))
}

fn expand_replacement(
    result: &mut Vec<u8>,
    replacement: &[u8],
    target: &[u8],
    captures: &crate::regex::Captures,
    allow_groups: bool,
) {
    let mut at = 0;
    while at < replacement.len() {
        let byte = replacement[at];
        at += 1;
        let group = if byte == b'&' {
            Some(0)
        } else if byte == b'\\' && at < replacement.len() {
            let escaped = replacement[at];
            if allow_groups && escaped.is_ascii_digit() {
                at += 1;
                Some(usize::from(escaped - b'0'))
            } else if escaped == b'&' || escaped == b'\\' || allow_groups {
                at += 1;
                result.push(escaped);
                None
            } else {
                result.push(byte);
                None
            }
        } else {
            result.push(byte);
            None
        };
        if let Some(group) = group {
            if let Some((start, end)) = captures.pos(group) {
                result.extend_from_slice(&target[start..end]);
            }
        }
    }
}

fn strtonum(value: &[u8]) -> f64 {
    let (base, digits) = if value.starts_with(b"0x") || value.starts_with(b"0X") {
        (16_u32, &value[2..])
    } else if value.first() == Some(&b'0') && value.get(1).is_some_and(u8::is_ascii_digit) {
        let digits = value;
        let count = digits
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits[..count]
            .iter()
            .any(|byte| matches!(byte, b'8' | b'9'))
            || digits
                .get(count)
                .is_some_and(|byte| matches!(byte, b'.' | b'e' | b'E'))
        {
            return crate::value::parse_leading_number(value);
        }
        (8, digits)
    } else {
        let trimmed = value.trim_ascii_start();
        let unsigned = trimmed
            .strip_prefix(b"-")
            .or_else(|| trimmed.strip_prefix(b"+"))
            .unwrap_or(trimmed);
        if unsigned.starts_with(b"0x") || unsigned.starts_with(b"0X") {
            return 0.0;
        }
        return crate::value::parse_leading_number(value);
    };
    let mut number = 0.0;
    for &byte in digits {
        let Some(digit) = (byte as char).to_digit(base) else {
            break;
        };
        number = number * f64::from(base) + f64::from(digit);
    }
    number
}

/// Simplified mktime implementation (UTC-based)
fn simple_mktime(year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64) -> i64 {
    // Days in each month (non-leap year)
    const DAYS_IN_MONTH: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    fn is_leap_year(year: i64) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }

    fn days_in_year(year: i64) -> i64 {
        if is_leap_year(year) { 366 } else { 365 }
    }

    // Calculate days from epoch (1970-01-01)
    let mut days: i64 = 0;

    // Add days for complete years
    for y in 1970..year {
        days += days_in_year(y);
    }
    for y in year..1970 {
        days -= days_in_year(y);
    }

    // Add days for complete months in current year
    for m in 1..month {
        let m_idx = (m - 1) as usize;
        if m_idx < 12 {
            days += DAYS_IN_MONTH[m_idx];
            if m == 2 && is_leap_year(year) {
                days += 1;
            }
        }
    }

    // Add remaining days
    days += day - 1;

    // Convert to seconds
    days * 86400 + hour * 3600 + min * 60 + sec
}

/// Simplified strftime implementation
fn format_strftime(format: &[u8], timestamp: u64) -> BString {
    // Break down timestamp into components
    let secs = timestamp as i64;

    // Calculate year, month, day, etc.
    let (year, month, day, hour, min, sec, wday, yday) = breakdown_time(secs);

    let weekday_names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekday_full = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let month_full = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];

    let mut result = BString::default();
    let mut chars = format.iter().map(|byte| *byte as char).peekable();

    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.next() {
                Some('Y') => result.push_str(&format!("{:04}", year)),
                Some('y') => result.push_str(&format!("{:02}", year % 100)),
                Some('m') => result.push_str(&format!("{:02}", month)),
                Some('d') => result.push_str(&format!("{:02}", day)),
                Some('e') => result.push_str(&format!("{:2}", day)),
                Some('H') => result.push_str(&format!("{:02}", hour)),
                Some('M') => result.push_str(&format!("{:02}", min)),
                Some('S') => result.push_str(&format!("{:02}", sec)),
                Some('a') => result.push_str(weekday_names.get(wday as usize).unwrap_or(&"???")),
                Some('A') => result.push_str(weekday_full.get(wday as usize).unwrap_or(&"???")),
                Some('b') | Some('h') => {
                    result.push_str(month_names.get((month - 1) as usize).unwrap_or(&"???"))
                }
                Some('B') => {
                    result.push_str(month_full.get((month - 1) as usize).unwrap_or(&"???"))
                }
                Some('j') => result.push_str(&format!("{:03}", yday)),
                Some('u') => result.push_str(&format!("{}", if wday == 0 { 7 } else { wday })),
                Some('w') => result.push_str(&format!("{}", wday)),
                Some('Z') => result.push_str("UTC"),
                Some('z') => result.push_str("+0000"),
                Some('%') => result.push(b'%'),
                Some('n') => result.push(b'\n'),
                Some('t') => result.push(b'\t'),
                Some(c) => {
                    result.push(b'%');
                    result.push(c as u8);
                }
                None => result.push(b'%'),
            }
        } else {
            result.push(ch as u8);
        }
    }

    result
}

/// Break down epoch seconds into date/time components
fn breakdown_time(secs: i64) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    const DAYS_IN_MONTH: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    fn is_leap_year(year: i64) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }

    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let mut days = secs / 86400;

    // wday: 0 = Sunday, 1970-01-01 was Thursday (4)
    let wday = ((days + 4) % 7 + 7) % 7;

    // Calculate year
    let mut year = 1970i64;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days >= days_in_year {
            days -= days_in_year;
            year += 1;
        } else if days < 0 {
            year -= 1;
            let days_in_year = if is_leap_year(year) { 366 } else { 365 };
            days += days_in_year;
        } else {
            break;
        }
    }

    let yday = days + 1; // 1-based day of year

    // Calculate month and day
    let mut month = 1i64;
    for (m, &dim) in DAYS_IN_MONTH.iter().enumerate() {
        let mut days_in_month = dim;
        if m == 1 && is_leap_year(year) {
            days_in_month += 1;
        }
        if days < days_in_month {
            month = m as i64 + 1;
            break;
        }
        days -= days_in_month;
    }
    let day = days + 1;

    (year, month, day, hour, min, sec, wday, yday)
}
