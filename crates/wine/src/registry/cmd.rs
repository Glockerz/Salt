//! Registry operations that go through Wine's `reg` and `regedit`.

use std::path::Path;

use super::{Data, Key};
use crate::{Error, Prefix, Result};

fn format_data(data: &Data) -> Option<(&'static str, String)> {
	Some(match data {
		Data::Sz(s) => ("REG_SZ", s.clone()),
		Data::MultiSz(v) => ("REG_MULTI_SZ", format!("{}\0\0", v.join("\0"))),
		Data::Dword(d) => ("REG_DWORD", d.to_string()),
		Data::Qword(q) => ("REG_QWORD", q.to_string()),
		Data::Binary(b) => ("REG_BINARY", b.iter().map(|b| format!("{b:02x}")).collect()),
		_ => return None,
	})
}

impl Prefix {
	fn reg(&self, args: &[&str]) -> Result<Vec<u8>> {
		let (out, status) = self.wine("reg", args).quiet().output()?;
		if status.success() {
			return Ok(out);
		}
		// reg(1) reports errors on stdout.
		if let Some(msg) = out.strip_prefix(b"reg: ") {
			return Err(Error::Registry(String::from_utf8_lossy(msg).trim_end().to_owned()));
		}
		Err(Error::Exit {
			program: "reg".into(),
			status,
		})
	}

	/// Adds a value to the live registry. An empty `value` sets (Default).
	pub fn registry_add(&self, key: &str, value: &str, data: impl Into<Data>) -> Result<()> {
		if key.is_empty() {
			return Err(Error::Registry("no registry key given".into()));
		}
		let data = data.into();
		let (t, d) = format_data(&data).ok_or_else(|| Error::Registry(format!("unhandled type {data:?}")))?;
		let mut args = vec!["add", key, "/t", t, "/d", &d, "/f"];
		if value.is_empty() {
			args.push("/ve");
		} else {
			args.extend(["/v", value]);
		}
		self.reg(&args).map(drop)
	}

	/// Deletes a key, or one of its values, from the live registry.
	pub fn registry_delete(&self, key: &str, value: &str) -> Result<()> {
		if key.is_empty() {
			return Err(Error::Registry("no registry key given".into()));
		}
		let mut args = vec!["delete", key, "/f"];
		if !value.is_empty() {
			args.extend(["/v", value]);
		}
		self.reg(&args).map(drop)
	}

	/// Imports a .reg file into the live registry.
	pub fn registry_import_file(&self, name: &Path) -> Result<()> {
		self.wine("regedit", [std::ffi::OsStr::new("/C"), name.as_os_str()]).quiet().run()
	}

	/// Imports a key tree into the live registry. The key must be a
	/// top-level registry key such as `HKEY_CURRENT_USER`.
	pub fn registry_import_key(&self, key: &Key) -> Result<()> {
		self.wine("regedit", ["/C", "-"]).quiet().stdin(key.export_bytes()).run()
	}
}
