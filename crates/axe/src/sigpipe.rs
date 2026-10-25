//! Preserve the SIGPIPE disposition inherited by the AXE process.
//!
//! Rust's Unix runtime changes SIGPIPE to SIG_IGN before `main` runs.  A small
//! ELF/Mach-O initializer records the disposition before that change;
//! `initialize` publishes it for child-process setup.

#[cfg(unix)]
mod unix {
    use std::sync::atomic::{AtomicU8, Ordering};

    const UNKNOWN: u8 = 0;
    const DEFAULT: u8 = 1;
    const IGNORE: u8 = 2;

    static INHERITED: AtomicU8 = AtomicU8::new(UNKNOWN);

    extern "C" fn capture_inherited() {
        // `signal` is used instead of inspecting `sigaction` so this remains
        // portable across Unix libc layouts.  The initializer runs before the
        // Rust runtime starts and before AXE creates any threads.
        let previous = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
        if previous == libc::SIG_ERR {
            return;
        }
        let _ = unsafe { libc::signal(libc::SIGPIPE, previous) };
        let disposition = if previous == libc::SIG_IGN {
            IGNORE
        } else {
            // A caught handler is reset to SIG_DFL by exec, so it has the same
            // post-exec semantics as the default disposition here.
            DEFAULT
        };
        INHERITED.store(disposition, Ordering::Relaxed);
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    #[used]
    #[unsafe(link_section = ".init_array")]
    static CAPTURE_INIT: extern "C" fn() = capture_inherited;

    #[cfg(target_os = "macos")]
    #[used]
    #[unsafe(link_section = "__DATA,__mod_init_func")]
    static CAPTURE_INIT: extern "C" fn() = capture_inherited;

    fn disposition() -> Option<(libc::sighandler_t, bool)> {
        match INHERITED.load(Ordering::Relaxed) {
            DEFAULT => Some((libc::SIG_DFL, false)),
            IGNORE => Some((libc::SIG_IGN, true)),
            _ => None,
        }
    }

    pub(super) fn initialize() {
        if let Some((_, ignored)) = disposition() {
            brush_shell::bundled::set_child_sigpipe_ignored(ignored);
        }
    }

    pub(super) fn restore_for_applet() {
        if let Some((handler, _)) = disposition() {
            // SAFETY: this runs immediately before an applet starts and before
            // that applet can create worker threads.
            let _ = unsafe { libc::signal(libc::SIGPIPE, handler) };
        }
    }
}

#[cfg(not(unix))]
pub(super) fn initialize() {}

#[cfg(not(unix))]
pub(super) fn restore_for_applet() {}

pub(crate) fn initialize() {
    #[cfg(unix)]
    unix::initialize();
}

pub(crate) fn restore_for_applet() {
    #[cfg(unix)]
    unix::restore_for_applet();
}
