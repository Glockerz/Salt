//! Wine registry string escaping, matching server/registry.c and
//! programs/regedit.

use std::fmt::Write;

fn is_xdigit(c: u8) -> bool {
	c.is_ascii_hexdigit()
}

fn hex_val(c: u8) -> u32 {
	(c as char).to_digit(16).unwrap_or(0)
}

/// Unescapes a quoted registry string or key path.
pub fn unescape(src: &str) -> String {
	if !src.contains('\\') {
		return src.to_owned();
	}
	let b = src.as_bytes();
	let mut out = String::with_capacity(src.len());
	let mut high: Option<u32> = None;
	let mut i = 0;

	let push = |out: &mut String, high: &mut Option<u32>, cur: u32| {
		if let Some(h) = high.take() {
			if (0xdc00..=0xdfff).contains(&cur) {
				let c = ((h - 0xd800) << 10) + (cur - 0xdc00) + 0x10000;
				out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
				return;
			}
			out.push('\u{FFFD}');
		}
		if (0xd800..=0xdbff).contains(&cur) {
			*high = Some(cur);
		} else {
			out.push(char::from_u32(cur).unwrap_or('\u{FFFD}'));
		}
	};

	while i < b.len() {
		if b[i] != b'\\' {
			let ch = src[i..].chars().next().unwrap();
			i += ch.len_utf8();
			push(&mut out, &mut high, ch as u32);
			continue;
		}
		i += 1;
		if i >= b.len() {
			break;
		}
		let c = b[i];
		let simple = match c {
			b'a' => Some('\x07'),
			b'b' => Some('\x08'),
			b'e' => Some('\x1b'),
			b'f' => Some('\x0c'),
			b'n' => Some('\n'),
			b'r' => Some('\r'),
			b't' => Some('\t'),
			b'v' => Some('\x0b'),
			b'\\' => Some('\\'),
			b'"' => Some('"'),
			_ => None,
		};
		if let Some(s) = simple {
			out.push(s);
			i += 1;
			continue;
		}
		match c {
			b'0'..=b'7' => {
				let mut val = 0u32;
				let mut k = 0;
				while k < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
					val = (val << 3) | (b[i] - b'0') as u32;
					i += 1;
					k += 1;
				}
				out.push(char::from_u32(val).unwrap_or('\u{FFFD}'));
			}
			b'x' => {
				i += 1;
				if i >= b.len() || !is_xdigit(b[i]) {
					out.push('x');
					continue;
				}
				let mut cur = 0u32;
				let mut k = 0;
				while k < 4 && i < b.len() && is_xdigit(b[i]) {
					cur = (cur << 4) | hex_val(b[i]);
					i += 1;
					k += 1;
				}
				push(&mut out, &mut high, cur);
			}
			_ => {
				// Invalid escape, kept verbatim.
				out.push('\\');
				let ch = src[i..].chars().next().unwrap();
				out.push(ch);
				i += ch.len_utf8();
			}
		}
	}
	if high.is_some() {
		out.push('\u{FFFD}');
	}
	out
}

/// Escapes a registry string.
///
/// `quote` escapes double quotes. `raw` writes non-ASCII characters and
/// control characters as-is (regedit export), except newlines.
pub fn escape(src: &str, quote: bool, raw: bool) -> String {
	if src.is_empty() {
		return String::new();
	}
	let u16s: Vec<u16> = src.encode_utf16().collect();
	let mut out = String::with_capacity(src.len() * 2);
	let n = u16s.len();
	let mut i = 0;
	while i < n {
		let c = u16s[i];
		if raw && c > 127 {
			let len = if (0xd800..0xdc00).contains(&c) && i + 1 < n { 2 } else { 1 };
			out.extend(char::decode_utf16(u16s[i..i + len].iter().copied()).map(|r| r.unwrap_or('\u{FFFD}')));
			i += len;
			continue;
		}
		if c > 127 {
			if i + 1 < n && u16s[i + 1] < 128 && is_xdigit(u16s[i + 1] as u8) {
				let _ = write!(out, "\\x{c:04x}");
			} else {
				let _ = write!(out, "\\x{c:x}");
			}
			i += 1;
			continue;
		}
		let ch = c as u8;
		if ch < 32 && raw {
			if ch == b'\n' {
				out.push_str("\\n");
			} else {
				out.push(ch as char);
			}
			i += 1;
			continue;
		}
		if ch < 32 {
			match ch {
				0x07 => out.push_str("\\a"),
				0x08 => out.push_str("\\b"),
				b'\t' => out.push_str("\\t"),
				b'\n' => out.push_str("\\n"),
				0x0b => out.push_str("\\v"),
				0x0c => out.push_str("\\f"),
				b'\r' => out.push_str("\\r"),
				0x1b => out.push_str("\\e"),
				_ => {
					if i + 1 < n && (b'0' as u16..=b'7' as u16).contains(&u16s[i + 1]) {
						let _ = write!(out, "\\{ch:03o}");
					} else {
						let _ = write!(out, "\\{ch:o}");
					}
				}
			}
			i += 1;
			continue;
		}
		if (!raw || quote) && ch == b'\\' {
			out.push('\\');
		}
		if quote && ch == b'"' {
			out.push('\\');
		}
		if !quote && !raw && (ch == b'[' || ch == b']') {
			out.push('\\');
		}
		out.push(ch as char);
		i += 1;
	}
	out
}
