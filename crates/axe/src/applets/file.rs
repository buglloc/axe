use std::ffi::OsString;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{ArgAction, Parser};
use file_format::FileFormat;

const TEXT_SAMPLE_LIMIT: u64 = 64 * 1024;

#[derive(Parser)]
#[command(name = "file", version, about = "Determine file type")]
struct Args {
    /// Omit the file name from output.
    #[arg(short = 'b', long)]
    brief: bool,

    /// Print media type and character set.
    #[arg(short = 'i', long)]
    mime: bool,

    /// Print only the media type.
    #[arg(long)]
    mime_type: bool,

    /// Follow symbolic links.
    #[arg(short = 'L', long)]
    dereference: bool,

    #[arg(required = true, action = ArgAction::Append)]
    files: Vec<PathBuf>,
}

pub fn file(args: Vec<OsString>) -> i32 {
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            let code = if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                0
            } else {
                2
            };
            let _ = error.print();
            return code;
        }
    };

    let width = args
        .files
        .iter()
        .map(|path| path.display().to_string().len())
        .max()
        .unwrap_or(0);
    for path in &args.files {
        let result = describe(path, args.dereference, args.mime, args.mime_type);
        match result {
            Ok(description) => print_description(path, &description, args.brief, width),
            Err(error) => {
                let reason = error
                    .to_string()
                    .split_once(" (os error")
                    .map_or_else(|| error.to_string(), |(reason, _)| reason.to_string());
                let description = format!("cannot open `{}' ({reason})", path.display());
                print_description(path, &description, args.brief, width);
            }
        }
    }
    // GNU file exits zero even when files cannot be opened; only -E fails hard.
    0
}

fn print_description(path: &Path, description: &str, brief: bool, width: usize) {
    if brief {
        println!("{description}");
    } else {
        let name = format!("{}:", path.display());
        let pad = (width + 2).saturating_sub(name.len()).max(1);
        println!("{}{:pad$}{description}", name, "");
    }
}

fn describe(path: &Path, dereference: bool, mime: bool, mime_type: bool) -> io::Result<String> {
    if path == Path::new("-") {
        let mut bytes = Vec::new();
        io::stdin().read_to_end(&mut bytes)?;
        if bytes.is_empty() {
            return Ok("empty".into());
        }

        let format = FileFormat::from_reader(Cursor::new(&bytes))?;
        return Ok(format_description(format, &bytes, true, mime, mime_type));
    }

    let metadata = if dereference {
        fs::metadata(path)?
    } else {
        fs::symlink_metadata(path)?
    };
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(path)?;
        if mime || mime_type {
            return Ok(mime_line("inode/symlink", mime));
        }
        return Ok(format!("symbolic link to {}", target.display()));
    }

    if file_type.is_dir() {
        if mime || mime_type {
            return Ok(mime_line("inode/directory", mime));
        }
        return Ok("directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let special = if file_type.is_socket() {
            Some(("socket".to_string(), "inode/socket"))
        } else if file_type.is_fifo() {
            Some(("fifo (named pipe)".to_string(), "inode/fifo"))
        } else if file_type.is_block_device() {
            Some((device_text(path, "block special"), "inode/blockdevice"))
        } else if file_type.is_char_device() {
            Some((device_text(path, "character special"), "inode/chardevice"))
        } else {
            None
        };

        if let Some((text, mime_name)) = special {
            if mime || mime_type {
                return Ok(mime_line(mime_name, mime));
            }
            return Ok(text);
        }
    }

    if metadata.len() == 0 {
        if mime || mime_type {
            return Ok(mime_line("application/x-empty", mime));
        }
        return Ok("empty".into());
    }

    let format = FileFormat::from_file(path)?;
    let (sample, complete) = if format == FileFormat::PlainText {
        let mut sample = Vec::with_capacity(TEXT_SAMPLE_LIMIT as usize + 1);
        fs::File::open(path)?
            .take(TEXT_SAMPLE_LIMIT + 1)
            .read_to_end(&mut sample)?;
        let complete = sample.len() <= TEXT_SAMPLE_LIMIT as usize;
        sample.truncate(TEXT_SAMPLE_LIMIT as usize);
        (sample, complete)
    } else {
        (Vec::new(), false)
    };
    Ok(format_description(
        format, &sample, complete, mime, mime_type,
    ))
}

fn format_description(
    format: FileFormat,
    sample: &[u8],
    complete: bool,
    mime: bool,
    mime_type: bool,
) -> String {
    let plain_text = format == FileFormat::PlainText;
    let json =
        plain_text && complete && serde_json::from_slice::<serde_json::Value>(sample).is_ok();
    let media_type = if json {
        "application/json"
    } else {
        format.media_type()
    };

    if mime {
        let charset = if plain_text {
            if sample.is_ascii() {
                "us-ascii"
            } else {
                "utf-8"
            }
        } else {
            "binary"
        };
        format!("{media_type}; charset={charset}")
    } else if mime_type {
        media_type.into()
    } else if json {
        "JSON text data".into()
    } else if format == FileFormat::ArbitraryBinaryData {
        "data".into()
    } else if plain_text && sample.is_ascii() {
        "ASCII text".into()
    } else if plain_text {
        "Unicode text, UTF-8 text".into()
    } else {
        format.name().into()
    }
}

/// GNU appends "; charset=binary" to inode and empty MIME types for `file -i`
/// (but not `file --mime-type`).
fn mime_line(kind: &str, mime: bool) -> String {
    if mime {
        format!("{kind}; charset=binary")
    } else {
        kind.to_string()
    }
}

/// GNU file appends the major/minor numbers to device nodes.
fn device_text(path: &Path, kind: &str) -> String {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path) {
        Ok(meta) => {
            let rdev = meta.rdev();
            // Linux packed encoding of major/minor device numbers.
            let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfffu64);
            let minor = (rdev & 0xff) | ((rdev >> 12) & !0xffu64);
            format!("{kind} ({major}/{minor})")
        }
        Err(_) => kind.to_string(),
    }
}
