//! Minimal PE helpers.

use std::path::Path;

use object::read::pe::{PeFile32, PeFile64};
use object::{Object, ObjectSection};

use crate::{Error, Result};

fn invalid(e: object::Error) -> Error {
	Error::Other(format!("pe: {e}"))
}

/// Checks that the file is a well-formed PE image, used to detect
/// partial or corrupted downloads.
pub fn validate(name: &Path) -> Result<()> {
	let data = std::fs::read(name)?;
	validate_bytes(&data)
}

pub fn validate_bytes(data: &[u8]) -> Result<()> {
	match PeFile64::parse(data) {
		Ok(_) => Ok(()),
		Err(_) => PeFile32::parse(data).map(drop).map_err(invalid),
	}
}

/// Returns a copy of the named section's data.
pub fn section(data: &[u8], name: &str) -> Result<Vec<u8>> {
	fn find<'a>(f: &impl Object<'a>, name: &str) -> Option<Vec<u8>> {
		let s = f.section_by_name(name)?;
		s.data().ok().map(<[u8]>::to_vec)
	}
	let found = match PeFile64::parse(data) {
		Ok(f) => find(&f, name),
		Err(_) => find(&PeFile32::parse(data).map_err(invalid)?, name),
	};
	found.ok_or_else(|| Error::Other(format!("pe: missing section {name}")))
}
