//! Install and bootstrap Roblox binary deployments.
//!
//! Ported from `github.com/sewnie/rbxbin`. Installation is restructured
//! for speed: mirrors are probed concurrently, the package manifest and
//! the installer are fetched in parallel, and packages are hashed while
//! they download instead of being re-read afterwards.

mod deployment;
mod fflags;
mod install;
mod mirror;
mod package;

pub use deployment::{Deployment, get_deployment};
pub use fflags::{FFlag, FFlags, apply as apply_fflags};
pub use install::{Progress, install};
pub use mirror::{DEFAULT_MIRROR, MIRRORS, Mirror};
pub use package::{Package, PackageDirectories, md5_file, parse_packages, scan_package_directories};
pub use rbxweb::BinaryType;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("deployment channel is invalid or unauthorized")]
	BadChannel,
	#[error("no accessible deploy mirror found")]
	NoMirror,
	#[error("package manifest is invalid")]
	InvalidManifest,
	#[error("unhandled package manifest version: {0}")]
	ManifestVersion(String),
	#[error("could not locate package directory map in installer")]
	DirMapScan,
	#[error("unhandled package: {0}")]
	UnhandledPackage(String),
	#[error("package {name} is corrupted (expected {expected}, got {got})")]
	Checksum { name: String, expected: String, got: String },
	#[error("illegal package file path: {0}")]
	IllegalPath(String),
	#[error("{url}: bad status {status}")]
	Status { url: String, status: u16 },
	#[error("zip: {0}")]
	Zip(#[from] zip::result::ZipError),
	#[error("pe: {0}")]
	Pe(String),
	#[error(transparent)]
	Api(#[from] rbxweb::Error),
	#[error("http: {0}")]
	Http(#[from] Box<ureq::Error>),
	#[error(transparent)]
	Io(#[from] std::io::Error),
	#[error("cancelled")]
	Cancelled,
}

impl From<ureq::Error> for Error {
	fn from(e: ureq::Error) -> Self {
		Error::Http(Box::new(e))
	}
}

pub type Result<T> = std::result::Result<T, Error>;

/// Application settings required for Roblox to run.
pub const APP_SETTINGS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\
	<Settings>\r\n        <ContentFolder>content</ContentFolder>\r\n        \
	<BaseUrl>http://www.roblox.com</BaseUrl>\r\n</Settings>\r\n";

/// Writes `AppSettings.xml` into a deployment directory.
pub fn write_app_settings(dir: &std::path::Path) -> std::io::Result<()> {
	std::fs::write(dir.join("AppSettings.xml"), APP_SETTINGS)
}
