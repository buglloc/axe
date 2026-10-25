use std::ffi::CStr;
use std::io;
use std::ptr;

use uucore::libc;

pub(super) struct SearchLocale {
    locale: libc::locale_t,
    previous: libc::locale_t,
}

impl SearchLocale {
    pub(super) fn new() -> io::Result<Self> {
        // An unavailable environment locale uses C, as at CLI startup.
        Self::activate(c"").or_else(|_| Self::bytes())
    }

    pub(super) fn bytes() -> io::Result<Self> {
        Self::activate(c"C")
    }

    fn activate(name: &CStr) -> io::Result<Self> {
        let mask = libc::LC_CTYPE_MASK | libc::LC_COLLATE_MASK;
        // SAFETY: name is NUL-terminated. A null base creates an independently
        // owned locale without changing process-global state.
        let locale = unsafe { libc::newlocale(mask, name.as_ptr(), ptr::null_mut()) };
        if locale.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: locale is live and uniquely owned. uselocale changes only the
        // calling thread, and this guard does not cross an await or thread boundary.
        let previous = unsafe { libc::uselocale(locale) };
        if previous.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: activation failed, so no thread uses the owned locale.
            unsafe { libc::freelocale(locale) };
            return Err(error);
        }
        Ok(Self { locale, previous })
    }
}

impl Drop for SearchLocale {
    fn drop(&mut self) {
        // SAFETY: the guard stays on its originating thread. The previous
        // locale remains live and must be restored before the owned one is freed.
        let restored = unsafe { libc::uselocale(self.previous) };
        if !restored.is_null() {
            // SAFETY: the owned locale is no longer active on any thread.
            unsafe { libc::freelocale(self.locale) };
        }
    }
}
