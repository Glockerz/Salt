//! DXVK management for a Wineprefix.

use std::io::Read;
use std::path::Path;

use crate::{Error, Prefix, Result, pe};

/// DLL overrides that make Wine prefer native (DXVK) Direct3D DLLs.
pub const OVERRIDES: &str = "d3d9,d3d10core,d3d11,dxgi=n,b";

/// Appends the DXVK DLL overrides to the prefix's `WINEDLLOVERRIDES`.
pub fn env_override(pfx: &mut Prefix) {
	match pfx.env.iter_mut().find(|(k, _)| k == "WINEDLLOVERRIDES") {
		Some((_, v)) => {
			v.push(';');
			v.push_str(OVERRIDES);
		}
		None => pfx.env.push(("WINEDLLOVERRIDES".into(), OVERRIDES.into())),
	}
}

/// Removes DXVK DLLs installed into the prefix's system directories and
/// lets wineboot restore Wine's own.
pub fn restore(pfx: &Prefix) -> Result<()> {
	for dir in ["syswow64", "system32"] {
		for name in ["d3d8", "d3d9", "d3d10core", "d3d11", "dxgi"] {
			let dll = pfx.dir().join("drive_c/windows").join(dir).join(format!("{name}.dll"));
			match std::fs::remove_file(dll) {
				Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
				_ => {}
			}
		}
	}
	pfx.wine("wineboot", ["-u"]).run()
}

/// Tarball URL for a DXVK version. "Sarek-" versions come from
/// DXVK-Sarek, and a "-async" suffix selects its async variant.
pub fn url(ver: &str) -> String {
	if let Some(v) = ver.strip_prefix("Sarek-") {
		let (v, name) = match v.strip_suffix("-async") {
			Some(v) => (v, "dxvk-sarek-async"),
			None => (v, "dxvk-sarek"),
		};
		return format!("https://github.com/pythonlover02/DXVK-Sarek/releases/download/v{v}/{name}-v{v}.tar.gz");
	}
	format!("https://github.com/doitsujin/dxvk/releases/download/v{ver}/dxvk-{ver}.tar.gz")
}

/// DXVK version of the prefix's system32 d3d11.dll.
pub fn version(pfx: &Prefix) -> Result<String> {
	dll_version(&pfx.dir().join("drive_c/windows/system32/d3d11.dll"))
}

/// Presumed DXVK version of a d3d11/d3d9/dxgi DLL, found by scanning its
/// .rdata section. Returns an empty string for non-DXVK DLLs.
pub fn dll_version(name: &Path) -> Result<String> {
	let data = std::fs::read(name)?;
	let rdata = pe::section(&data, ".rdata")?;
	Ok(scan_version(&rdata).unwrap_or_default())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
	hay.windows(needle.len()).position(|w| w == needle)
}

fn scan_version(b: &[u8]) -> Option<String> {
	// "Game: " always appears right before the DXVK version info.
	const HEAD: &[u8] = b"Game: \0";
	let info_start = find(b, HEAD)? + HEAD.len();
	let info_end = info_start + b[info_start..].iter().position(|&c| c == 0)?;
	let ver_end = b[info_end + 1..].iter().position(|&c| c == 0)?;
	// Skip the 'v' prefix and the NUL.
	let version = String::from_utf8_lossy(b.get(info_end + 2..info_end + ver_end + 1)?).into_owned();
	let prefix = b.get(info_start..info_end.checked_sub(2)?)?;
	Some(match prefix.strip_prefix(b"DXVK-") {
		Some(variant) => format!("{}-{version}", String::from_utf8_lossy(variant)),
		None => version,
	})
}

/// Reports whether two DXVK version strings name the same release.
/// DLLs report Sarek builds as "Sarek-Async-1.11.0" while the
/// configuration calls them "Sarek-1.11.0-async".
pub fn same_version(a: &str, b: &str) -> bool {
	let norm = |s: &str| {
		let mut v: Vec<String> = s.split('-').map(str::to_ascii_lowercase).collect();
		v.sort();
		v
	};
	norm(a) == norm(b)
}

/// Extracts the x64 DLLs of a DXVK tarball into `dir`.
pub fn extract_x64(tarball: impl Read, dir: &Path) -> Result<Vec<String>> {
	let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
	let mut installed = Vec::new();
	std::fs::create_dir_all(dir)?;
	for entry in ar.entries()? {
		let mut entry = entry?;
		if entry.header().entry_type() != tar::EntryType::Regular {
			continue;
		}
		let path = entry.path()?.into_owned();
		if path.extension().and_then(|e| e.to_str()) != Some("dll") {
			continue;
		}
		let parent = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str());
		if parent != Some("x64") {
			log::debug!("Ignoring DXVK installation entry {}", path.display());
			continue;
		}
		let Some(name) = path.file_name() else { continue };
		let dst = dir.join(name);
		log::debug!("Installing DXVK DLL locally to {}", dst.display());
		let mut f = std::fs::File::create(&dst)?;
		std::io::copy(&mut entry, &mut f).map_err(Error::Io)?;
		installed.push(name.to_string_lossy().into_owned());
	}
	Ok(installed)
}

#[cfg(test)]
mod tests {
	#[test]
	fn urls() {
		assert_eq!(
			super::url("2.7.1"),
			"https://github.com/doitsujin/dxvk/releases/download/v2.7.1/dxvk-2.7.1.tar.gz"
		);
		assert_eq!(
			super::url("Sarek-1.11.0-async"),
			"https://github.com/pythonlover02/DXVK-Sarek/releases/download/v1.11.0/dxvk-sarek-async-v1.11.0.tar.gz"
		);
	}

	#[test]
	fn scan() {
		let mut b = b"junk\0Game: \0DXVK-Sarek-Async: \0v1.11.0\0more".to_vec();
		assert_eq!(super::scan_version(&b).as_deref(), Some("Sarek-Async-1.11.0"));
		b = b"x\0Game: \0DXVK: \0v2.7.1\0".to_vec();
		assert_eq!(super::scan_version(&b).as_deref(), Some("2.7.1"));
		b = b"Game: \0".to_vec();
		assert_eq!(super::scan_version(&b), None);
	}
}
