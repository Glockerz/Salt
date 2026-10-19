use std::path::Path;

use super::{Data, HEADER_EXPORT, HEADER_WINE, Key, unescape};
use crate::{Error, Result};

pub(super) fn parse_file(name: &Path) -> Result<Key> {
	let b = std::fs::read(name)?;
	parse(&String::from_utf8_lossy(&b))
}

fn err(line: usize, msg: impl Into<String>) -> Error {
	Error::Parse {
		line: line + 1,
		msg: msg.into(),
	}
}

/// Parses a Wine internal (`user.reg`) or regedit-exported registry file.
///
/// Wine's files name the root key through their `;; All keys relative to`
/// directive; exported files leave the root unnamed and keep full paths.
///
/// Unlike the Go original, lines have no length limit: Studio stores its
/// window layout as one escaped XML string, which can exceed the 64 KiB
/// bufio.Scanner default and make the whole registry unreadable.
pub fn parse(src: &str) -> Result<Key> {
	let src = src.strip_prefix('\u{feff}').unwrap_or(src);
	let mut lines = src.lines().map(|l| l.strip_suffix('\r').unwrap_or(l)).enumerate();

	match lines.next() {
		Some((_, HEADER_WINE | HEADER_EXPORT)) => {}
		Some((n, h)) => return Err(err(n, format!("expected registry header, got {h:?}"))),
		None => return Err(err(0, "empty registry")),
	}

	let mut root = Key::default();
	// Index path of the current key; stays valid as keys are only appended
	// while it is set, and it is reset on deletions.
	let mut cur: Option<Vec<usize>> = None;

	fn resolve<'a>(root: &'a mut Key, idx: &[usize]) -> &'a mut Key {
		idx.iter().fold(root, |k, &i| &mut k.subkeys[i])
	}

	while let Some((n, line)) = lines.next() {
		let Some(first) = line.bytes().next() else {
			continue;
		};
		match first {
			b';' => {
				let Some(rest) = line.strip_prefix(";; All keys relative to ") else {
					continue;
				};
				if !root.name.is_empty() {
					return Err(err(n, "unexpected path directive"));
				}
				let path = rest.trim();
				root.name = if path.starts_with(r"REGISTRY\\User\\") && path != r"REGISTRY\\User\\.Default" {
					"HKEY_CURRENT_USER".into()
				} else if path == r"REGISTRY\\Machine" {
					"HKEY_LOCAL_MACHINE".into()
				} else {
					return Err(err(n, format!("unknown registry path: {path}")));
				};
			}
			b'#' => {
				let Some(idx) = &cur else { continue };
				if let Some(raw) = line.strip_prefix("#time=") {
					let t = i64::from_str_radix(raw.trim(), 16).map_err(|e| err(n, format!("time: {e}")))?;
					resolve(&mut root, idx).modified = t;
				} else if line == "#link" {
					resolve(&mut root, idx).link = true;
				}
			}
			b'[' => {
				let end = line.rfind(']').filter(|&i| i > 0).ok_or_else(|| err(n, "unterminated key"))?;
				let path = unescape(&line[1..end]);
				if let Some(path) = path.strip_prefix('-') {
					root.delete(path);
					cur = None;
					continue;
				}
				let mut idx = Vec::new();
				let mut k = &mut root;
				for seg in path.split('\\') {
					let i = match k.subkeys.iter().rposition(|s| s.name == seg) {
						Some(i) => i,
						None => {
							k.subkeys.push(Key::named(seg));
							k.subkeys.len() - 1
						}
					};
					idx.push(i);
					k = &mut k.subkeys[i];
				}
				cur = Some(idx);
			}
			b'"' | b'@' => {
				let Some(idx) = &cur else {
					return Err(err(n, "value without key"));
				};
				let mut line = std::borrow::Cow::Borrowed(line);
				// Hex data continues over lines ending with a backslash.
				while line.ends_with('\\') {
					let mut s = line.into_owned();
					s.pop();
					match lines.next() {
						Some((_, next)) => s.push_str(next.trim()),
						None => {}
					}
					line = std::borrow::Cow::Owned(s);
				}
				let (name, raw) = split_value(&line).ok_or_else(|| err(n, "malformed value"))?;
				let data = parse_data(raw).map_err(|m| err(n, format!("{name}: {m}")))?;
				let key = resolve(&mut root, idx);
				match data {
					Data::Delete => {
						key.delete_value(name);
					}
					d => {
						key.set(name, d);
					}
				}
			}
			_ => {}
		}
	}
	Ok(root)
}

/// Splits `"name"=data` or `@=data`, keeping the name escaped.
fn split_value(line: &str) -> Option<(&str, &str)> {
	if let Some(rest) = line.strip_prefix("@=") {
		return Some(("", rest));
	}
	let b = line.as_bytes();
	let mut i = 1;
	while i < b.len() {
		match b[i] {
			b'\\' => i += 2,
			b'"' => break,
			_ => i += 1,
		}
	}
	if i >= b.len() || b.get(i + 1) != Some(&b'=') {
		return None;
	}
	Some((&line[1..i], &line[i + 2..]))
}

fn unquote(s: &str) -> Result<String, String> {
	let inner = s
		.strip_prefix('"')
		.and_then(|s| s.strip_suffix('"'))
		.ok_or("unterminated string")?;
	Ok(unescape(inner))
}

fn parse_data(value: &str) -> Result<Data, String> {
	match value.bytes().next() {
		None => return Err("expected data".into()),
		Some(b'"') => return Ok(Data::Sz(unquote(value)?)),
		Some(b'-') => return Ok(Data::Delete),
		_ => {}
	}
	let (prefix, data) = value.split_once(':').ok_or("missing type")?;
	match prefix {
		"dword" => return u32::from_str_radix(data, 16).map(Data::Dword).map_err(|e| format!("dword: {e}")),
		"str(2)" => return Ok(Data::ExpandSz(unquote(data)?)),
		"str(7)" => {
			let s = unquote(data)?;
			let mut v: Vec<String> = s.split('\0').map(str::to_owned).collect();
			v.pop(); // foo\0bar\0 -> [foo, bar, ""]
			return Ok(Data::MultiSz(v));
		}
		_ => {}
	}
	if !prefix.starts_with("hex") {
		return Err(format!("unhandled data type: {prefix}"));
	}
	let hex = parse_bytes(data)?;
	let need = |n: usize| {
		if hex.len() < n {
			Err(format!("{prefix}: expected {n} bytes"))
		} else {
			Ok(())
		}
	};
	Ok(match prefix {
		"hex" | "hex(3)" => Data::Binary(hex),
		"hex(1)" => Data::BinaryString(hex),
		"hex(2)" => Data::ExpandSz(decode_w(&hex)),
		"hex(4)" => {
			need(4)?;
			Data::DwordLe(u32::from_le_bytes(hex[..4].try_into().unwrap()))
		}
		"hex(5)" => {
			need(4)?;
			Data::DwordBe(u32::from_be_bytes(hex[..4].try_into().unwrap()))
		}
		"hex(6)" => Data::Link(decode_w(&hex)),
		"hex(7)" => {
			let mut v: Vec<String> = decode_w(&hex).split('\0').map(str::to_owned).collect();
			v.pop();
			Data::MultiSz(v)
		}
		"hex(b)" => {
			need(8)?;
			Data::Qword(u64::from_le_bytes(hex[..8].try_into().unwrap()))
		}
		name => {
			let id = name
				.strip_prefix("hex(")
				.and_then(|s| s.strip_suffix(')'))
				.ok_or_else(|| format!("unsupported hex type: {name}"))?;
			let id = u32::from_str_radix(id, 16).map_err(|e| format!("{name}: {e}"))?;
			Data::Internal { id, data: hex }
		}
	})
}

fn parse_bytes(s: &str) -> Result<Vec<u8>, String> {
	let mut buf = Vec::with_capacity(s.len() / 3 + 1);
	for tok in s.split(',') {
		let tok = tok.trim();
		if tok.is_empty() {
			continue;
		}
		if tok.starts_with('\\') {
			break;
		}
		buf.push(u8::from_str_radix(tok, 16).map_err(|e| format!("hex {tok:?}: {e}"))?);
	}
	Ok(buf)
}

fn decode_w(b: &[u8]) -> String {
	let mut u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
	if u.last() == Some(&0) {
		u.pop();
	}
	String::from_utf16_lossy(&u)
}
