//! Salt updating itself from its GitHub releases.
//!
//! Salt ships as a single-file flatpak bundle, which has no remote that
//! `flatpak update` could pull from. Instead Salt downloads the newer
//! bundle and has the host's flatpak reinstall it, which keeps the
//! user's data. The running Salt is unaffected; the next launch uses
//! the new version.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::dirs::DIRS;
use crate::netutil::{self, Result};
use crate::sysinfo::INFO;

const ASSET: &str = "salt.flatpak";

fn parse(v: &str) -> Option<(u32, u32, u32)> {
	let mut it = v.trim_start_matches('v').splitn(3, '.').map(|p| p.parse().ok());
	Some((it.next()??, it.next()??, it.next()??))
}

/// Whether release `tag` is newer than version `cur`.
fn newer(tag: &str, cur: &str) -> bool {
	matches!((parse(tag), parse(cur)), (Some(t), Some(c)) if t > c)
}

/// The installation Salt runs from, as a flatpak option.
fn installation() -> &'static str {
	let info = fs::read_to_string("/.flatpak-info").unwrap_or_default();
	let system = info
		.lines()
		.find_map(|l| l.strip_prefix("app-path="))
		.is_some_and(|p| p.starts_with("/var/lib/flatpak/"));
	if system { "--system" } else { "--user" }
}

fn sha256(path: &Path) -> Result<String> {
	let mut f = fs::File::open(path)?;
	let mut h = Sha256::new();
	let mut buf = vec![0u8; 256 * 1024];
	loop {
		let n = f.read(&mut buf)?;
		if n == 0 {
			break;
		}
		h.update(&buf[..n]);
	}
	Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Installs the latest release if it is newer than this one. Returns
/// the version installed, or None when Salt is up to date or isn't a
/// flatpak.
pub fn update() -> Result<Option<String>> {
	static BUSY: AtomicBool = AtomicBool::new(false);
	if !INFO.flatpak || BUSY.swap(true, Ordering::SeqCst) {
		return Ok(None);
	}
	let r = install_latest();
	BUSY.store(false, Ordering::SeqCst);
	r
}

fn install_latest() -> Result<Option<String>> {
	let cur = env!("CARGO_PKG_VERSION");
	let rel = netutil::salt_release()?;
	if !newer(&rel.tag_name, cur) {
		log::info!("Salt is up to date version={cur} latest={}", rel.tag_name);
		return Ok(None);
	}
	let ver = rel.tag_name.trim_start_matches('v').to_owned();
	let asset = rel
		.assets
		.iter()
		.find(|a| a.name == ASSET)
		.ok_or_else(|| format!("release {} has no {ASSET}", rel.tag_name))?;

	log::info!("Downloading Salt update version={ver}");
	fs::create_dir_all(&DIRS.downloads)?;
	let file = DIRS.downloads.join(format!("salt-{ver}.flatpak"));
	netutil::download(&asset.browser_download_url, &file, &|_| {})?;
	let r = verify_and_install(&file, asset.digest.as_deref());
	let _ = fs::remove_file(&file);
	r?;
	log::info!("Installed Salt update version={ver}");
	Ok(Some(ver))
}

fn verify_and_install(file: &Path, digest: Option<&str>) -> Result<()> {
	match digest.and_then(|d| d.strip_prefix("sha256:")) {
		Some(want) => {
			let got = sha256(file)?;
			if !got.eq_ignore_ascii_case(want) {
				return Err(format!("update checksum mismatch: got {got}, want {want}").into());
			}
		}
		None => log::warn!("Salt update has no published checksum, installing unverified"),
	}

	// The cache directory is the same path on the host.
	let out = Command::new("flatpak-spawn")
		.args(["--host", "flatpak", "install", installation()])
		.args(["--noninteractive", "--assumeyes", "--reinstall", "--bundle"])
		.arg(file)
		.output()?;
	if !out.status.success() {
		let msg = String::from_utf8_lossy(&out.stderr);
		return Err(format!("flatpak install: {}", msg.trim()).into());
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn versions() {
		assert!(newer("v2.0.7", "2.0.6"));
		assert!(newer("v2.1.0", "2.0.9"));
		assert!(newer("3.0.0", "2.10.10"));
		assert!(!newer("v2.0.6", "2.0.6"));
		assert!(!newer("v2.0.5", "2.0.6"));
		assert!(!newer("nightly", "2.0.6"));
		assert!(!newer("v2.0", "2.0.6"));
	}

	/// Downloads the latest release and checks it against its digest.
	#[test]
	#[ignore = "network"]
	fn latest_release() {
		let rel = netutil::salt_release().unwrap();
		let asset = rel.assets.iter().find(|a| a.name == ASSET).unwrap();
		let want = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).unwrap();
		let file = std::env::temp_dir().join("salt-selfupdate-test.flatpak");
		netutil::download(&asset.browser_download_url, &file, &|_| {}).unwrap();
		let got = sha256(&file).unwrap();
		let _ = fs::remove_file(&file);
		assert_eq!(got, want);
	}
}
