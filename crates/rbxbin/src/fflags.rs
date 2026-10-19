use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A Fast Flag value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FFlag {
	Bool(bool),
	Int(i64),
	Float(f64),
	String(String),
}

/// Roblox Fast Flags, kept sorted so output is stable.
pub type FFlags = BTreeMap<String, FFlag>;

/// Writes the flags to `ClientSettings/ClientAppSettings.json` in a
/// deployment directory.
pub fn apply(flags: &FFlags, version_dir: &Path) -> std::io::Result<()> {
	let dir = version_dir.join("ClientSettings");
	std::fs::create_dir_all(&dir)?;
	let json = serde_json::to_vec_pretty(flags).map_err(std::io::Error::other)?;
	std::fs::write(dir.join("ClientAppSettings.json"), json)
}

