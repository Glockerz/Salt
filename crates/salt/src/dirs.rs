//! XDG directory layout. The directories keep Salt's names, so Salt
//! shares data, prefixes and configuration with existing Salt installs.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

fn xdg(var: &str, fallback: &str) -> PathBuf {
	std::env::var_os(var)
		.map(PathBuf::from)
		.filter(|p| p.is_absolute())
		.unwrap_or_else(|| home().join(fallback))
}

pub fn home() -> PathBuf {
	std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

pub struct Dirs {
	pub cache: PathBuf,
	pub config: PathBuf,
	pub data: PathBuf,
	pub overlays: PathBuf,
	pub downloads: PathBuf,
	pub logs: PathBuf,
	pub prefixes: PathBuf,
	pub versions: PathBuf,
	pub config_path: PathBuf,
	pub wine_path: PathBuf,
	pub appdata_path: PathBuf,
}

pub const TAG_PREFIX: &str = "kombucha-";

pub static DIRS: LazyLock<Dirs> = LazyLock::new(|| {
	let cache = xdg("XDG_CACHE_HOME", ".cache").join("vinegar");
	let config = xdg("XDG_CONFIG_HOME", ".config").join("vinegar");
	let data = xdg("XDG_DATA_HOME", ".local/share").join("vinegar");
	Dirs {
		overlays: config.join("overlays"),
		downloads: cache.join("downloads"),
		logs: cache.join("logs"),
		prefixes: data.join("prefixes"),
		versions: data.join("versions"),
		config_path: config.join("config.toml"),
		wine_path: data.join("kombucha"),
		appdata_path: data.join("appdata"),
		cache,
		config,
		data,
	}
});

/// Converts an absolute host path to its Wine Z: drive path.
pub fn windows(name: &Path) -> String {
	assert!(name.is_absolute(), "dirs: unhandled local path");
	format!("Z:{}", name.display().to_string().replace('/', "\\"))
}
