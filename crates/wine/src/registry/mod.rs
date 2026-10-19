//! Offline Wine registry model, parser and regedit exporter.
//!
//! Unlike the Go original, keys do not hold parent pointers; paths are
//! threaded through the recursive operations that need them.

mod cmd;
mod escape;
mod export;
mod parse;

pub use escape::{escape, unescape};

use std::path::Path;

use crate::Result;

pub(crate) const HEADER_WINE: &str = "WINE REGISTRY Version 2";
pub(crate) const HEADER_EXPORT: &str = "Windows Registry Editor Version 5.00";

/// Data of a registry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
	/// REG_SZ
	Sz(String),
	/// REG_EXPAND_SZ, hex(2) / str(2)
	ExpandSz(String),
	/// REG_MULTI_SZ, hex(7) / str(7)
	MultiSz(Vec<String>),
	/// REG_DWORD
	Dword(u32),
	/// REG_QWORD, hex(b)
	Qword(u64),
	/// REG_BINARY, hex
	Binary(Vec<u8>),
	/// hex(1)
	BinaryString(Vec<u8>),
	/// REG_DWORD_LITTLE_ENDIAN, hex(4)
	DwordLe(u32),
	/// REG_DWORD_BIG_ENDIAN, hex(5)
	DwordBe(u32),
	/// REG_LINK, hex(6)
	Link(String),
	/// DEVPROP_TYPE_*, hex(ffff????)
	Internal { id: u32, data: Vec<u8> },
	/// Deletes the value when exported.
	Delete,
}

impl From<&str> for Data {
	fn from(s: &str) -> Self {
		Data::Sz(s.to_owned())
	}
}

impl From<String> for Data {
	fn from(s: String) -> Self {
		Data::Sz(s)
	}
}

impl From<u32> for Data {
	fn from(v: u32) -> Self {
		Data::Dword(v)
	}
}

impl Data {
	pub fn as_str(&self) -> Option<&str> {
		match self {
			Data::Sz(s) | Data::ExpandSz(s) => Some(s),
			_ => None,
		}
	}

	pub fn as_bytes(&self) -> Option<&[u8]> {
		match self {
			Data::Binary(b) | Data::BinaryString(b) => Some(b),
			_ => None,
		}
	}
}

/// A registry value. The (Default) value has an empty name.
///
/// Names are kept exactly as they appear in registry files (escaped),
/// which makes parse-then-export round trips lossless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
	pub name: String,
	pub data: Data,
}

/// An offline registry key with its values and subkeys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Key {
	pub name: String,
	pub values: Vec<Value>,
	pub subkeys: Vec<Key>,
	/// FILETIME of the last modification, from Wine's `#time=`.
	pub modified: i64,
	/// Symbolic link keys are skipped when exporting for regedit.
	pub link: bool,
}

fn canonical_root(name: &str) -> &str {
	match name {
		"HKLM" => "HKEY_LOCAL_MACHINE",
		"HKCU" => "HKEY_CURRENT_USER",
		n => n,
	}
}

impl Key {
	pub fn named(name: impl Into<String>) -> Self {
		Key {
			name: name.into(),
			..Default::default()
		}
	}

	/// Creates a root key and the given absolute path below it, returning
	/// the root. `HKCU\Foo\Bar` yields `HKEY_CURRENT_USER` → `Foo` → `Bar`.
	pub fn with_path(path: &str) -> Self {
		let (root, rest) = path.split_once('\\').unwrap_or((path, ""));
		let mut k = Key::named(canonical_root(root));
		k.add(rest);
		k
	}

	pub fn value(&self, name: &str) -> Option<&Value> {
		self.values.iter().find(|v| v.name == name)
	}

	/// Sets a value, replacing the data of an existing value of that name.
	pub fn set(&mut self, name: impl Into<String>, data: impl Into<Data>) -> &mut Self {
		let name = name.into();
		let data = data.into();
		match self.values.iter_mut().find(|v| v.name == name) {
			Some(v) => v.data = data,
			None => self.values.push(Value { name, data }),
		}
		self
	}

	pub fn delete_value(&mut self, name: &str) -> bool {
		let len = self.values.len();
		self.values.retain(|v| v.name != name);
		len != self.values.len()
	}

	/// Finds the key at the relative path.
	pub fn query(&self, path: &str) -> Option<&Key> {
		if path.is_empty() {
			return Some(self);
		}
		let mut cur = self;
		for seg in path.split('\\') {
			// Most recently added keys are last, which is the common
			// case while parsing.
			cur = cur.subkeys.iter().rev().find(|k| k.name == seg)?;
		}
		Some(cur)
	}

	pub fn query_mut(&mut self, path: &str) -> Option<&mut Key> {
		if path.is_empty() {
			return Some(self);
		}
		let mut cur = self;
		for seg in path.split('\\') {
			cur = cur.subkeys.iter_mut().rev().find(|k| k.name == seg)?;
		}
		Some(cur)
	}

	/// Finds the key at the relative path, creating missing keys.
	pub fn add(&mut self, path: &str) -> &mut Key {
		if path.is_empty() {
			return self;
		}
		let mut cur = self;
		for seg in path.split('\\') {
			let idx = match cur.subkeys.iter().rposition(|k| k.name == seg) {
				Some(i) => i,
				None => {
					cur.subkeys.push(Key::named(seg));
					cur.subkeys.len() - 1
				}
			};
			cur = &mut cur.subkeys[idx];
		}
		cur
	}

	/// Places a copy of `key` at the relative `path`, replacing the
	/// values there and merging subkeys.
	pub fn add_key(&mut self, path: &str, key: &Key) -> &mut Key {
		let dst = self.add(path);
		dst.values = key.values.clone();
		dst.modified = key.modified;
		dst.link = key.link;
		for sk in &key.subkeys {
			dst.add_key(&sk.name, sk);
		}
		dst
	}

	/// Removes the key at the relative path.
	pub fn delete(&mut self, path: &str) -> bool {
		let (parent, name) = match path.rsplit_once('\\') {
			Some((p, n)) => (p, n),
			None => ("", path),
		};
		let Some(parent) = self.query_mut(parent) else {
			return false;
		};
		match parent.subkeys.iter().rposition(|k| k.name == name) {
			Some(i) => {
				parent.subkeys.remove(i);
				true
			}
			None => false,
		}
	}
}

/// A Wineprefix's HKEY_CURRENT_USER and HKEY_LOCAL_MACHINE, as read
/// from `user.reg` and `system.reg`.
#[derive(Debug, Clone, Default)]
pub struct Registry {
	pub current_user: Key,
	pub machine: Key,
}

impl Registry {
	pub fn load(prefix: &Path) -> Result<Self> {
		Ok(Registry {
			machine: parse::parse_file(&prefix.join("system.reg"))?,
			current_user: parse::parse_file(&prefix.join("user.reg"))?,
		})
	}

	/// Finds an absolute key path rooted at HKLM or HKCU.
	pub fn query(&self, path: &str) -> Option<&Key> {
		let (root, key) = path.split_once('\\').unwrap_or((path, ""));
		match canonical_root(root) {
			"HKEY_LOCAL_MACHINE" => self.machine.query(key),
			"HKEY_CURRENT_USER" => self.current_user.query(key),
			_ => None,
		}
	}
}

pub use parse::parse;

#[cfg(test)]
mod tests;
