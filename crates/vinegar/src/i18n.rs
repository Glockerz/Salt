//! gettext translations through glibc, which GtkBuilder also uses for
//! translatable UI strings.

use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;

unsafe extern "C" {
	fn gettext(msgid: *const c_char) -> *mut c_char;
	fn bindtextdomain(domain: *const c_char, dir: *const c_char) -> *mut c_char;
	fn bind_textdomain_codeset(domain: *const c_char, codeset: *const c_char) -> *mut c_char;
	fn textdomain(domain: *const c_char) -> *mut c_char;
}

const DOMAIN: &CStr = c"vinegar";

/// Locale directory: next to the executable (`../share/locale`) for
/// relocatable installs, otherwise the build-time location.
fn locale_dir() -> PathBuf {
	if let Some(dir) = std::env::current_exe()
		.ok()
		.and_then(|e| e.parent().map(|p| p.join("../share/locale")))
		.filter(|d| d.is_dir())
	{
		return dir;
	}
	PathBuf::from(option_env!("VINEGAR_LOCALEDIR").unwrap_or("/usr/share/locale"))
}

pub fn init() {
	let dir = CString::new(locale_dir().to_string_lossy().into_owned()).unwrap_or_default();
	// SAFETY: plain C calls with valid NUL-terminated strings.
	unsafe {
		libc::setlocale(libc::LC_ALL, c"".as_ptr());
		bindtextdomain(DOMAIN.as_ptr(), dir.as_ptr());
		bind_textdomain_codeset(DOMAIN.as_ptr(), c"UTF-8".as_ptr());
		textdomain(DOMAIN.as_ptr());
	}
}

/// Translates a message.
pub fn l(msg: &str) -> String {
	let Ok(c) = CString::new(msg) else {
		return msg.to_owned();
	};
	// SAFETY: gettext returns either its argument or a static string.
	unsafe { CStr::from_ptr(gettext(c.as_ptr())) }
		.to_string_lossy()
		.into_owned()
}
