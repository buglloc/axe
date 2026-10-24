use std::io;

/// An I/O error without std's appended errno number.
pub(crate) fn strerror(error: &io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(index) if error.raw_os_error().is_some() => text[..index].to_owned(),
        _ => text,
    }
}
