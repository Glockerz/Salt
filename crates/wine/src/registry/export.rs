use std::fmt::Write as _;
use std::io::{self, Write};

use super::{Data, HEADER_EXPORT, Key, Value, escape};

fn encode_w(s: &str) -> Vec<u8> {
	s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

impl Key {
	/// Writes a regedit export of this key, treating it as a root key.
	///
	/// Keys with neither values nor subkeys are exported as deletions,
	/// and values with [`Data::Delete`] delete the value. Link keys are
	/// skipped.
	pub fn export(&self, w: &mut impl Write) -> io::Result<()> {
		let mut out = String::with_capacity(4096);
		out.push_str(HEADER_EXPORT);
		out.push('\n');
		self.export_into(&mut out, &self.name);
		w.write_all(out.as_bytes())
	}

	/// Returns the regedit export as bytes.
	pub fn export_bytes(&self) -> Vec<u8> {
		let mut v = Vec::new();
		self.export(&mut v).expect("writing to a Vec cannot fail");
		v
	}

	fn export_into(&self, out: &mut String, path: &str) {
		if self.link {
			return;
		}
		if self.values.is_empty() && self.subkeys.is_empty() {
			let _ = write!(out, "\n[-{}]\n", escape(path, false, true));
			return;
		}
		if !self.values.is_empty() {
			let _ = write!(out, "\n[{}]\n", escape(path, false, true));
		}
		for v in &self.values {
			v.export_into(out);
		}
		for sk in &self.subkeys {
			let sub = format!("{path}\\{}", sk.name);
			sk.export_into(out, &sub);
		}
	}
}

impl Value {
	fn export_into(&self, out: &mut String) {
		let start = out.len();
		if self.name.is_empty() {
			out.push_str("@=");
		} else {
			let _ = write!(out, "\"{}\"=", self.name);
		}
		// Column tracking for line wrapping, as regedit does.
		let mut pos = out.len() - start + 6;
		let payload: Vec<u8> = match &self.data {
			Data::Delete => {
				out.push('-');
				Vec::new()
			}
			Data::Sz(s) => {
				let _ = write!(out, "\"{}\"", escape(s, true, true));
				Vec::new()
			}
			Data::ExpandSz(s) => {
				out.push_str("hex(2):");
				encode_w(&format!("{s}\0"))
			}
			Data::MultiSz(v) => {
				out.push_str("hex(7):");
				let mut s = v.join("\0");
				s.push_str("\0\0");
				encode_w(&s)
			}
			Data::Dword(d) => {
				let _ = write!(out, "dword:{d:08x}");
				Vec::new()
			}
			Data::Qword(q) => {
				out.push_str("hex(b):");
				q.to_le_bytes().to_vec()
			}
			Data::Binary(b) => {
				out.push_str("hex:");
				pos -= 3;
				b.clone()
			}
			Data::BinaryString(b) => {
				out.push_str("hex(1):");
				b.clone()
			}
			Data::DwordLe(d) => {
				out.push_str("hex(4):");
				d.to_le_bytes().to_vec()
			}
			Data::DwordBe(d) => {
				out.push_str("hex(5):");
				d.to_be_bytes().to_vec()
			}
			Data::Link(s) => {
				out.push_str("hex(6):");
				encode_w(s)
			}
			Data::Internal { id, data } => {
				let _ = write!(out, "hex({id:08x}):");
				pos += 7;
				data.clone()
			}
		};
		for (i, b) in payload.iter().enumerate() {
			let _ = write!(out, "{b:02x}");
			pos += 3;
			if i < payload.len() - 1 {
				out.push(',');
				if pos + 1 > 76 {
					out.push_str("\\\n  ");
					pos = 2;
				}
			}
		}
		out.push('\n');
	}
}
