use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use md5::{Digest, Md5};

use crate::{Error, Result};

/// A deployment package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
	pub name: String,
	pub checksum: String,
	pub size: u64,
	pub zip_size: u64,
}

/// Where each package of a deployment is extracted to, relative to the
/// deployment directory.
pub type PackageDirectories = HashMap<String, String>;

/// Parses a `rbxPkgManifest.txt`.
pub fn parse_packages(s: &str) -> Result<Vec<Package>> {
	let m: Vec<&str> = s.split("\r\n").collect();
	if m.len() < 2 || (m.len() - 2) % 4 != 0 {
		return Err(Error::InvalidManifest);
	}
	if m[0] != "v0" {
		return Err(Error::ManifestVersion(m[0].into()));
	}
	// Skip the version line and the trailing empty element.
	m[1..m.len() - 1]
		.chunks_exact(4)
		.map(|c| {
			Ok(Package {
				name: c[0].into(),
				checksum: c[1].into(),
				zip_size: c[2].parse().map_err(|_| Error::InvalidManifest)?,
				size: c[3].parse().map_err(|_| Error::InvalidManifest)?,
			})
		})
		.collect()
}

/// Finds the package directory JSON map embedded in the installer's
/// .rdata section: a NUL-delimited `{"name":"dir\\", ...}` object.
pub fn scan_package_directories(b: &[u8]) -> Option<PackageDirectories> {
	let mut start = 0;
	let mut i = 1;
	while i + 1 < b.len() {
		if b[i] == b'{' && b[i + 1] == b'"' && b[i - 1] == 0 {
			start = i;
		}
		if start > 0 && i >= 2 && b[i - 1] == b'}' && b[i - 2] == b'"' && b[i] == 0 {
			match serde_json::from_slice::<PackageDirectories>(&b[start..i]) {
				Ok(mut pd) => {
					for d in pd.values_mut() {
						*d = clean(&d.replace('\\', "/"));
					}
					return Some(pd);
				}
				Err(_) => start = 0,
			}
		}
		i += 1;
	}
	None
}

/// Lexically cleans a slash-separated relative path like Go's path.Clean.
fn clean(p: &str) -> String {
	let mut out: Vec<&str> = Vec::new();
	for seg in p.split('/') {
		match seg {
			"" | "." => {}
			".." => {
				if out.last().is_some_and(|s| *s != "..") {
					out.pop();
				} else {
					out.push("..");
				}
			}
			s => out.push(s),
		}
	}
	if out.is_empty() { ".".into() } else { out.join("/") }
}

/// MD5 of a file as lowercase hex.
pub fn md5_file(path: &Path) -> io::Result<String> {
	let mut f = fs::File::open(path)?;
	let mut h = Md5::new();
	let mut buf = vec![0u8; 1 << 20];
	loop {
		let n = f.read(&mut buf)?;
		if n == 0 {
			break;
		}
		h.update(&buf[..n]);
	}
	Ok(hex(&h.finalize()))
}

pub(crate) fn hex(b: &[u8]) -> String {
	b.iter().map(|b| format!("{b:02x}")).collect()
}

impl Package {
	/// Checks a downloaded package file against its checksum.
	pub fn verify(&self, src: &Path) -> Result<()> {
		let got = md5_file(src)?;
		if got != self.checksum {
			return Err(Error::Checksum {
				name: src.display().to_string(),
				expected: self.checksum.clone(),
				got,
			});
		}
		Ok(())
	}

	/// Extracts a downloaded package into `dir`. Packages that are not
	/// zip archives are copied into it under the package's name.
	///
	/// Returns the number of bytes written.
	pub fn extract(&self, src: &Path, dir: &Path, progress: &dyn Fn(u64)) -> Result<u64> {
		let f = fs::File::open(src)?;
		let mut ar = match zip::ZipArchive::new(io::BufReader::with_capacity(256 * 1024, f)) {
			Ok(ar) => ar,
			Err(zip::result::ZipError::InvalidArchive(_)) if !self.name.ends_with(".zip") => {
				return self.extract_file(src, dir);
			}
			Err(e) => return Err(e.into()),
		};
		fs::create_dir_all(dir)?;

		let mut written = 0;
		let mut buf = vec![0u8; 256 * 1024];
		for i in 0..ar.len() {
			let mut entry = ar.by_index(i)?;
			// Roblox uses backslashes in entry names.
			let name = String::from_utf8_lossy(entry.name_raw()).replace('\\', "/");
			let rel = safe_relative(&name).ok_or_else(|| Error::IllegalPath(name.clone()))?;
			if rel.as_os_str().is_empty() {
				continue;
			}
			let dest = dir.join(&rel);
			if entry.is_dir() || name.ends_with('/') {
				fs::create_dir_all(&dest)?;
				continue;
			}
			if let Some(p) = dest.parent() {
				fs::create_dir_all(p)?;
			}
			let mut out = fs::File::create(&dest)?;
			loop {
				let n = entry.read(&mut buf)?;
				if n == 0 {
					break;
				}
				io::Write::write_all(&mut out, &buf[..n])?;
				written += n as u64;
				progress(n as u64);
			}
		}
		Ok(written)
	}

	fn extract_file(&self, src: &Path, dir: &Path) -> Result<u64> {
		fs::create_dir_all(dir)?;
		let n = fs::copy(src, dir.join(&self.name))?;
		if n != self.size {
			return Err(io::Error::new(io::ErrorKind::WriteZero, "short write").into());
		}
		Ok(n)
	}
}

/// Resolves an entry name below the destination, rejecting parent
/// traversal ("zip slip"). Leading slashes are ignored, as Roblox
/// packages contain entries such as `\` and `\content\...`.
fn safe_relative(name: &str) -> Option<PathBuf> {
	let mut out = PathBuf::new();
	for c in Path::new(name.trim_start_matches('/')).components() {
		match c {
			Component::Normal(s) => out.push(s),
			Component::CurDir => {}
			_ => return None,
		}
	}
	Some(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn manifest() {
		let m = "v0\r\nRobloxApp.zip\r\nabc\r\n10\r\n20\r\ncontent-fonts.zip\r\ndef\r\n30\r\n40\r\n";
		let p = parse_packages(m).unwrap();
		assert_eq!(p.len(), 2);
		assert_eq!(p[1].name, "content-fonts.zip");
		assert_eq!(p[1].zip_size, 30);
		assert_eq!(p[1].size, 40);
		assert!(matches!(parse_packages("v1\r\n"), Err(Error::ManifestVersion(_))));
		assert!(matches!(parse_packages("v0\r\na\r\n"), Err(Error::InvalidManifest)));
	}

	#[test]
	fn dirs() {
		let mut b = b"\x00junk{\"x\x00".to_vec();
		b.extend_from_slice(b"\x00{\"RobloxApp.zip\":\"\\\\\",\"content-fonts.zip\":\"content\\\\fonts\\\\\"}\x00tail");
		let pd = scan_package_directories(&b).unwrap();
		assert_eq!(pd["RobloxApp.zip"], ".");
		assert_eq!(pd["content-fonts.zip"], "content/fonts");
	}

	#[test]
	fn paths() {
		assert_eq!(clean("a/b/../c/"), "a/c");
		assert!(safe_relative("../x").is_none());
		assert_eq!(safe_relative("/content/x").unwrap(), PathBuf::from("content/x"));
		assert_eq!(safe_relative("/").unwrap(), PathBuf::new());
		assert_eq!(safe_relative("a/./b").unwrap(), PathBuf::from("a/b"));
	}
}
