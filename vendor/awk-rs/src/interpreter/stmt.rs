use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};

use bstr::BString;

use crate::ast::*;
use crate::error::{Error, Result};
use crate::value::Value;

use super::{Interpreter, OutputFile};

/// Result of executing a statement
pub enum StmtResult {
    Normal,
    Break,
    Continue,
    Control,
    Return(Value),
}

impl<'a> Interpreter<'a> {
    pub fn execute_block<W: Write>(&mut self, block: &Block, output: &mut W) -> Result<StmtResult> {
        for stmt in &block.statements {
            let result = match self.execute_stmt(stmt, output) {
                Ok(result) => result,
                Err(Error::ControlFlow) => return Ok(StmtResult::Control),
                Err(error) => return Err(error),
            };
            if self.should_exit || self.should_next || self.should_nextfile {
                return Ok(StmtResult::Control);
            }
            match result {
                StmtResult::Normal => continue,
                other => return Ok(other),
            }
        }
        Ok(StmtResult::Normal)
    }

    pub fn execute_stmt<W: Write>(&mut self, stmt: &Stmt, output: &mut W) -> Result<StmtResult> {
        if self.should_exit || self.should_next || self.should_nextfile {
            return Ok(StmtResult::Control);
        }
        match stmt {
            Stmt::Empty => Ok(StmtResult::Normal),

            Stmt::Expr(expr) => {
                self.eval_expr_with_output(expr, output)?;
                Ok(StmtResult::Normal)
            }

            Stmt::Print {
                args,
                output: redirect,
                ..
            } => {
                self.execute_print(args, redirect, output)?;
                Ok(StmtResult::Normal)
            }

            Stmt::Printf {
                format,
                args,
                output: redirect,
                ..
            } => {
                self.execute_printf(format, args, redirect, output)?;
                Ok(StmtResult::Normal)
            }

            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                let cond = self.eval_expr_with_output(condition, output)?;
                if cond.is_truthy() {
                    self.execute_stmt(then_branch, output)
                } else if let Some(else_stmt) = else_branch {
                    self.execute_stmt(else_stmt, output)
                } else {
                    Ok(StmtResult::Normal)
                }
            }

            Stmt::While {
                condition, body, ..
            } => {
                loop {
                    let cond = self.eval_expr_with_output(condition, output)?;
                    if !cond.is_truthy() {
                        break;
                    }
                    match self.execute_stmt(body, output)? {
                        StmtResult::Normal | StmtResult::Continue => continue,
                        StmtResult::Break => break,
                        StmtResult::Control => return Ok(StmtResult::Control),
                        StmtResult::Return(v) => return Ok(StmtResult::Return(v)),
                    }
                }
                Ok(StmtResult::Normal)
            }

            Stmt::DoWhile {
                body, condition, ..
            } => {
                loop {
                    match self.execute_stmt(body, output)? {
                        StmtResult::Normal | StmtResult::Continue => {}
                        StmtResult::Break => break,
                        StmtResult::Control => return Ok(StmtResult::Control),
                        StmtResult::Return(v) => return Ok(StmtResult::Return(v)),
                    }
                    let cond = self.eval_expr_with_output(condition, output)?;
                    if !cond.is_truthy() {
                        break;
                    }
                }
                Ok(StmtResult::Normal)
            }

            Stmt::For {
                init,
                condition,
                update,
                body,
                ..
            } => {
                // Execute init
                if let Some(init_stmt) = init {
                    self.execute_stmt(init_stmt, output)?;
                }

                loop {
                    // Check condition
                    if let Some(cond_expr) = condition {
                        let cond = self.eval_expr_with_output(cond_expr, output)?;
                        if !cond.is_truthy() {
                            break;
                        }
                    }

                    // Execute body
                    match self.execute_stmt(body, output)? {
                        StmtResult::Normal | StmtResult::Continue => {}
                        StmtResult::Break => break,
                        StmtResult::Control => return Ok(StmtResult::Control),
                        StmtResult::Return(v) => return Ok(StmtResult::Return(v)),
                    }

                    // Execute update
                    if let Some(update_expr) = update {
                        self.eval_expr_with_output(update_expr, output)?;
                    }
                }
                Ok(StmtResult::Normal)
            }

            Stmt::ForIn {
                var, array, body, ..
            } => {
                // Get keys from array (resolve aliases for pass-by-reference)
                let resolved_array = self
                    .array_aliases
                    .get(array)
                    .map(|s| s.as_str())
                    .unwrap_or(array);
                let keys: Vec<BString> = self
                    .arrays
                    .get(resolved_array)
                    .map(|arr| arr.keys().cloned().collect())
                    .unwrap_or_default();

                for key in keys {
                    self.set_variable_value(var, Value::from_string(key))?;
                    match self.execute_stmt(body, output)? {
                        StmtResult::Normal | StmtResult::Continue => continue,
                        StmtResult::Break => break,
                        StmtResult::Control => return Ok(StmtResult::Control),
                        StmtResult::Return(v) => return Ok(StmtResult::Return(v)),
                    }
                }
                Ok(StmtResult::Normal)
            }

            Stmt::Block(block) => self.execute_block(block, output),

            Stmt::Break { .. } => Ok(StmtResult::Break),

            Stmt::Continue { .. } => Ok(StmtResult::Continue),

            Stmt::Next { .. } => {
                self.should_next = true;
                Ok(StmtResult::Control)
            }

            Stmt::Nextfile { .. } => {
                self.should_nextfile = true;
                Ok(StmtResult::Control)
            }

            Stmt::Exit { code, .. } => {
                if let Some(code) = code {
                    self.exit_code = self.eval_expr_with_output(code, output)?.to_number() as i32;
                }
                self.should_exit = true;
                Ok(StmtResult::Control)
            }

            Stmt::Return { value, .. } => {
                let val = value
                    .as_ref()
                    .map(|e| self.eval_expr_with_output(e, output))
                    .transpose()?
                    .unwrap_or(Value::Uninitialized);
                Ok(StmtResult::Return(val))
            }

            Stmt::Delete { array, index, .. } => {
                if index.is_empty() {
                    // delete array (entire array)
                    self.arrays.remove(array);
                } else {
                    let key_parts: Result<Vec<Value>> = index
                        .iter()
                        .map(|e| self.eval_expr_with_output(e, output))
                        .collect();
                    let key = self.make_array_key(&key_parts?);
                    self.delete_array_element(array, &key);
                }
                Ok(StmtResult::Normal)
            }
        }
    }

    fn execute_print<W: Write>(
        &mut self,
        args: &[Expr],
        redirect: &Option<OutputRedirect>,
        default_output: &mut W,
    ) -> Result<()> {
        let values: Result<Vec<BString>> = args
            .iter()
            .map(|expr| {
                let value = self.eval_expr_with_output(expr, default_output)?;
                Ok(self.to_output_str(&value))
            })
            .collect();
        let values = values?;
        if self.should_exit || self.should_next || self.should_nextfile {
            return Ok(());
        }
        let mut line = if values.is_empty() {
            self.record.clone()
        } else {
            super::input::join(&values, &self.ofs)
        };
        line.extend_from_slice(&self.ors);
        self.write_output(redirect, &line, default_output)?;

        Ok(())
    }

    fn execute_printf<W: Write>(
        &mut self,
        format_expr: &Expr,
        args: &[Expr],
        redirect: &Option<OutputRedirect>,
        default_output: &mut W,
    ) -> Result<()> {
        let format = self
            .eval_expr_with_output(format_expr, default_output)?
            .to_string_val();
        let values: Result<Vec<Value>> = args
            .iter()
            .map(|e| self.eval_expr_with_output(e, default_output))
            .collect();
        let values = values?;

        if self.should_exit || self.should_next || self.should_nextfile {
            return Ok(());
        }
        let formatted = self.format_printf(&format, &values);
        self.write_output(redirect, &formatted, default_output)?;

        Ok(())
    }

    fn write_output<W: Write>(
        &mut self,
        redirect: &Option<OutputRedirect>,
        bytes: &[u8],
        default_output: &mut W,
    ) -> Result<()> {
        match redirect {
            None => default_output.write_all(bytes)?,
            Some(OutputRedirect::Truncate(expr)) => {
                let filename = self
                    .eval_expr_with_output(expr, default_output)?
                    .to_string_val();
                if !self.should_exit {
                    self.get_or_open_file(&filename, false)?.write_all(bytes)?;
                }
            }
            Some(OutputRedirect::Append(expr)) => {
                let filename = self
                    .eval_expr_with_output(expr, default_output)?
                    .to_string_val();
                if !self.should_exit {
                    self.get_or_open_file(&filename, true)?.write_all(bytes)?;
                }
            }
            Some(OutputRedirect::Pipe(expr)) => {
                let command = self
                    .eval_expr_with_output(expr, default_output)?
                    .to_string_val();
                if !self.should_exit {
                    self.get_or_open_pipe(&command)?.write_all(bytes)?;
                }
            }
        }
        Ok(())
    }

    /// Get or open a file for output redirection
    fn get_or_open_file(&mut self, filename: &[u8], append: bool) -> Result<&mut OutputFile> {
        if !self.output_files.contains_key(filename) {
            let file = if append {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(OsStr::from_bytes(filename))
                    .map_err(Error::Io)?
            } else {
                File::create(OsStr::from_bytes(filename)).map_err(Error::Io)?
            };
            self.output_files
                .insert(filename.into(), OutputFile::File(file));
        }
        Ok(self.output_files.get_mut(filename).unwrap())
    }

    /// Get or open a pipe for output redirection
    fn get_or_open_pipe(&mut self, cmd: &[u8]) -> Result<&mut OutputFile> {
        if !self.output_files.contains_key(cmd) {
            let child = Command::new("sh")
                .arg("-c")
                .arg(OsStr::from_bytes(cmd))
                .stdin(Stdio::piped())
                .spawn()
                .map_err(Error::Io)?;

            let stdin = child.stdin.unwrap();
            self.output_files
                .insert(cmd.into(), OutputFile::Pipe(stdin));
        }
        Ok(self.output_files.get_mut(cmd).unwrap())
    }

    pub(crate) fn format_printf(&self, format: &[u8], args: &[Value]) -> BString {
        crate::fmt::sprintf(format, args, self.multibyte)
    }
}
