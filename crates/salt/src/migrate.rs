//! `salt fix-paths OLD_DATA_DIR`: after moving a Vinegar data directory
//! to Salt's, rewrites the absolute paths that Wine and Studio stored.
//!
//! Paths appear in three spellings: POSIX (`/home/u/...`), Windows
//! (`\home\u\...` after `Z:`), and Windows inside registry files, where
//! backslashes are doubled.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::dirs::DIRS;

fn replace_all(hay: &[u8], from: &[u8], to: &[u8]) -> (Vec<u8>, usize) {
	let mut out = Vec::with_capacity(hay.len());
	let mut i = 0;
	let mut n = 0;
	while i < hay.len() {
		if hay[i..].starts_with(from) {
			out.extend_from_slice(to);
			i += from.len();
			n += 1;
		} else {
			out.push(hay[i]);
			i += 1;
		}
	}
	(out, n)
}

/// Spellings of a path, as (old, new) pairs.
fn spellings(old: &str, new: &str) -> Vec<(String, String)> {
	let win = |p: &str| p.replace('/', "\\");
	let reg = |p: &str| p.replace('/', "\\\\");
	vec![(reg(old), reg(new)), (win(old), win(new)), (old.to_owned(), new.to_owned())]
}

/// Rewrites one file; returns the number of replacements.
pub fn fix_file(path: &Path, pairs: &[(String, String)]) -> std::io::Result<usize> {
	let mut data = match std::fs::read(path) {
		Ok(d) => d,
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
		Err(e) => return Err(e),
	};
	let mut total = 0;
	for (from, to) in pairs {
		let (d, n) = replace_all(&data, from.as_bytes(), to.as_bytes());
		data = d;
		total += n;
	}
	if total > 0 {
		let tmp = path.with_extension("salt-tmp");
		std::fs::write(&tmp, &data)?;
		std::fs::rename(tmp, path)?;
	}
	Ok(total)
}

fn files() -> Vec<PathBuf> {
	let mut v = vec![
		DIRS.data.join("settings.reg"),
		DIRS.config_path.clone(),
		DIRS.appdata_path.join("Roblox/mcp.bat"),
	];
	if let Ok(rd) = std::fs::read_dir(&DIRS.prefixes) {
		for p in rd.flatten() {
			for f in ["user.reg", "system.reg", "userdef.reg"] {
				v.push(p.path().join(f));
			}
		}
	}
	v
}

pub fn run(args: &[String]) -> ExitCode {
	crate::logging::init_stderr_only();
	let Some(old) = args.first() else {
		eprintln!("usage: salt fix-paths OLD_DATA_DIR");
		return ExitCode::FAILURE;
	};
	let new = DIRS.data.display().to_string();
	let old = old.trim_end_matches('/');
	if old == new {
		return ExitCode::SUCCESS;
	}
	let pairs = spellings(old, &new);
	let mut code = ExitCode::SUCCESS;
	for f in files() {
		match fix_file(&f, &pairs) {
			Ok(0) => {}
			Ok(n) => println!("{}: {n} paths updated", f.display()),
			Err(e) => {
				eprintln!("{}: {e}", f.display());
				code = ExitCode::FAILURE;
			}
		}
	}
	code
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rewrites_all_spellings() {
		let pairs = spellings("/home/u/.var/app/org.vinegarhq.Vinegar/data/vinegar", "/home/u/.var/app/io.github.glockerz.Salt/data/salt");
		let dir = std::env::temp_dir().join(format!("salt-fix-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let f = dir.join("user.reg");
		std::fs::write(
			&f,
			"\"Local AppData\"=str(2):\"Z:\\\\home\\\\u\\\\.var\\\\app\\\\org.vinegarhq.Vinegar\\\\data\\\\vinegar\\\\appdata\"\n\
			 set X=Z:\\home\\u\\.var\\app\\org.vinegarhq.Vinegar\\data\\vinegar\\versions\n\
			 wineroot = \"/home/u/.var/app/org.vinegarhq.Vinegar/data/vinegar/kombucha\"\n",
		)
		.unwrap();
		assert_eq!(fix_file(&f, &pairs).unwrap(), 3);
		let s = std::fs::read_to_string(&f).unwrap();
		assert!(s.contains("Z:\\\\home\\\\u\\\\.var\\\\app\\\\io.github.glockerz.Salt\\\\data\\\\salt\\\\appdata"));
		assert!(s.contains("Z:\\home\\u\\.var\\app\\io.github.glockerz.Salt\\data\\salt\\versions"));
		assert!(s.contains("/home/u/.var/app/io.github.glockerz.Salt/data/salt/kombucha"));
		assert!(!s.contains("vinegar"));
		std::fs::remove_dir_all(dir).unwrap();
	}
}
