//! Manage a Wineprefix and run Wine.
//!
//! A port of `github.com/sewnie/wine`, trimmed to what Salt uses and
//! extended with an offline registry API that avoids spawning Wine for
//! read-only queries.

mod cmd;
pub mod dxvk;
pub mod pe;
mod prefix;
pub mod registry;
pub mod webview2;

pub use cmd::{Cmd, LineSink};
pub use prefix::Prefix;
pub use registry::{Data, Key, Registry, Value};

/// Read the wineserver(1) man page for more details.
pub mod server {
	pub const DEBUG: &str = "--debug";
	pub const FOREGROUND: &str = "--foreground";
	pub const KILL: &str = "--kill";
	pub const PERSISTENT: &str = "--persistent";
	pub const WAIT: &str = "--wait";
}

/// Read the wineboot(1) man page for more details.
pub mod boot {
	pub const END_SESSION: &str = "--end-session";
	pub const FORCE_EXIT: &str = "--force";
	pub const INIT: &str = "--init";
	pub const KILL: &str = "--kill";
	pub const RESTART: &str = "--restart";
	pub const SHUTDOWN: &str = "--shutdown";
	pub const UPDATE: &str = "--update";
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("prefix directory is not an absolute path")]
	PrefixNotAbs,
	#[error("{0}: executable not found")]
	NotFound(String),
	#[error("{program} exited with {status}")]
	Exit {
		program: String,
		status: std::process::ExitStatus,
	},
	#[error("registry error: {0}")]
	Registry(String),
	#[error("registry parse: line {line}: {msg}")]
	Parse { line: usize, msg: String },
	#[error("http: {0}")]
	Http(#[from] Box<ureq::Error>),
	#[error(transparent)]
	Io(#[from] std::io::Error),
	#[error("{0}")]
	Other(String),
}

impl From<ureq::Error> for Error {
	fn from(e: ureq::Error) -> Self {
		Error::Http(Box::new(e))
	}
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
