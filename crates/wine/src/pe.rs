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

const PAGE: u32 = 0x1000;

fn up(n: u32, a: u32) -> u32 {
	n.div_ceil(a) * a
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
	d.get(o..o + 2)
		.map(|b| u16::from_le_bytes([b[0], b[1]]))
		.ok_or_else(|| Error::Other("pe: truncated".into()))
}

fn u32_at(d: &[u8], o: usize) -> Result<u32> {
	d.get(o..o + 4)
		.map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
		.ok_or_else(|| Error::Other("pe: truncated".into()))
}

fn put(d: &mut [u8], o: usize, v: u32) {
	d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// Where a PE image's headers and section table are.
struct Layout {
	opt: usize,
	/// Offsets of the data directory table and its entry count.
	dirs: usize,
	ndirs: u32,
	sections: usize,
	nsections: usize,
}

fn layout(d: &[u8]) -> Result<Layout> {
	if d.get(..2) != Some(b"MZ") {
		return Err(Error::Other("pe: not an image".into()));
	}
	let pe = u32_at(d, 0x3c)? as usize;
	if d.get(pe..pe + 4) != Some(b"PE\0\0") {
		return Err(Error::Other("pe: not an image".into()));
	}
	let nsections = u16_at(d, pe + 6)? as usize;
	let opt = pe + 24;
	let (dirs, ndirs) = match u16_at(d, opt)? {
		0x10b => (opt + 96, u32_at(d, opt + 92)?),
		0x20b => (opt + 112, u32_at(d, opt + 108)?),
		m => return Err(Error::Other(format!("pe: unknown optional header {m:#x}"))),
	};
	Ok(Layout {
		opt,
		dirs,
		ndirs,
		sections: opt + u16_at(d, pe + 20)? as usize,
		nsections,
	})
}

/// Whether every section's file data starts on a page boundary.
pub fn page_aligned(name: &Path) -> Result<bool> {
	use std::io::Read;
	let mut head = vec![0u8; 0x4000];
	let n = std::fs::File::open(name)?.read(&mut head)?;
	head.truncate(n);
	let l = layout(&head)?;
	for i in 0..l.nsections {
		let s = l.sections + 40 * i;
		if u32_at(&head, s + 20)? % PAGE != 0 {
			return Ok(false);
		}
	}
	Ok(true)
}

/// Rewrites a PE image so that each section's file data starts on a
/// page boundary (FileAlignment 0x1000), with the same contents once
/// loaded.
///
/// Wine maps a page-aligned section straight from the file, so every
/// process that loads the image shares one copy in the page cache. An
/// unaligned section is read into private memory instead: in each
/// process, which for WebView2's 300 MB msedge.dll costs hundreds of MB
/// per browser process. The signature is dropped, as it covers the
/// original layout. Returns the new image.
pub fn realign(d: &[u8]) -> Result<Vec<u8>> {
	let l = layout(d)?;
	let headers = u32_at(d, l.opt + 60)?;
	let mut out = d
		.get(..headers as usize)
		.ok_or_else(|| Error::Other("pe: truncated headers".into()))?
		.to_vec();
	out.resize(up(headers, PAGE) as usize, 0);

	// Old and new file offsets of each section's data, for moving the
	// debug directory's file pointers along.
	let mut moved: Vec<(u32, u32, u32)> = Vec::new();
	let mut order: Vec<usize> = (0..l.nsections).collect();
	order.sort_by_key(|&i| u32_at(d, l.sections + 40 * i + 20).unwrap_or(0));
	let mut end_of_sections = headers;
	for i in order {
		let s = l.sections + 40 * i;
		let size = u32_at(d, s + 16)?;
		let ptr = u32_at(d, s + 20)?;
		if size == 0 || ptr == 0 {
			continue;
		}
		let data = d
			.get(ptr as usize..(ptr + size) as usize)
			.ok_or_else(|| Error::Other(format!("pe: section {i} past end of file")))?;
		let new = out.len() as u32;
		out.extend_from_slice(data);
		out.resize(up(out.len() as u32, PAGE) as usize, 0);
		put(&mut out, s + 16, up(size, PAGE));
		put(&mut out, s + 20, new);
		moved.push((ptr, size, new));
		end_of_sections = end_of_sections.max(ptr + size);
	}

	// Data after the sections: the signature, which no longer applies,
	// then anything else, kept at the end.
	let mut overlay = (end_of_sections, d.len() as u32);
	if l.ndirs > 4 {
		let (cert, cert_size) = (u32_at(d, l.dirs + 32)?, u32_at(d, l.dirs + 36)?);
		if cert != 0 && cert >= end_of_sections {
			overlay.1 = overlay.1.min(cert);
			if cert + cert_size < d.len() as u32 {
				log::warn!("pe: dropping {} bytes after the signature", d.len() as u32 - cert - cert_size);
			}
		}
		put(&mut out, l.dirs + 32, 0);
		put(&mut out, l.dirs + 36, 0);
	}
	if overlay.1 > overlay.0 {
		let new = out.len() as u32;
		out.extend_from_slice(&d[overlay.0 as usize..overlay.1 as usize]);
		moved.push((overlay.0, overlay.1 - overlay.0, new));
	}
	let remap = |off: u32| {
		moved
			.iter()
			.find(|(old, size, _)| off >= *old && off < old + size)
			.map(|(old, _, new)| new + (off - old))
	};

	// IMAGE_DEBUG_DIRECTORY entries carry a file offset of their data.
	if l.ndirs > 6 {
		let (rva, size) = (u32_at(d, l.dirs + 48)?, u32_at(d, l.dirs + 52)?);
		if rva != 0
			&& let Some(at) = rva_to_offset(d, &l, rva)?
		{
			for e in 0..size / 28 {
				let field = at + e * 28 + 24;
				let Some(newfield) = remap(field) else { continue };
				let ptr = u32_at(d, field as usize)?;
				if ptr != 0 {
					put(&mut out, newfield as usize, remap(ptr).unwrap_or(0));
				}
			}
		}
	}

	put(&mut out, l.opt + 36, PAGE);
	put(&mut out, l.opt + 60, up(headers, PAGE));
	// The checksum is only checked for drivers.
	put(&mut out, l.opt + 64, 0);
	Ok(out)
}

fn rva_to_offset(d: &[u8], l: &Layout, rva: u32) -> Result<Option<u32>> {
	for i in 0..l.nsections {
		let s = l.sections + 40 * i;
		let (vsize, va, size, ptr) = (u32_at(d, s + 8)?, u32_at(d, s + 12)?, u32_at(d, s + 16)?, u32_at(d, s + 20)?);
		if rva >= va && rva < va + vsize.max(size) && rva - va < size {
			return Ok(Some(ptr + (rva - va)));
		}
	}
	Ok(None)
}

/// Realigns the image at `name` in place (see [`realign`]), unless it
/// already is. Returns whether it changed.
pub fn realign_file(name: &Path) -> Result<bool> {
	if page_aligned(name)? {
		return Ok(false);
	}
	let new = realign(&std::fs::read(name)?)?;
	validate_bytes(&new)?;
	let tmp = name.with_extension("realign.tmp");
	std::fs::write(&tmp, &new)?;
	// A rename replaces only this name; other hard links to the file
	// keep the original.
	std::fs::rename(&tmp, name)?;
	Ok(true)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sections(d: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
		let l = layout(d).unwrap();
		// The debug directory's file pointers are meant to change.
		let mut skip = Vec::new();
		if l.ndirs > 6 {
			let (rva, size) = (u32_at(d, l.dirs + 48).unwrap(), u32_at(d, l.dirs + 52).unwrap());
			skip.extend((0..size / 28).map(|e| rva + e * 28 + 24));
		}
		(0..l.nsections)
			.map(|i| {
				let s = l.sections + 40 * i;
				let name = String::from_utf8_lossy(&d[s..s + 8]).trim_end_matches('\0').to_owned();
				let (vsize, va, size, ptr) = (
					u32_at(d, s + 8).unwrap(),
					u32_at(d, s + 12).unwrap(),
					u32_at(d, s + 16).unwrap(),
					u32_at(d, s + 20).unwrap(),
				);
				// What the loader maps: the raw data up to the virtual size,
				// zero-filled.
				let mut v = d[ptr as usize..(ptr + size.min(vsize)) as usize].to_vec();
				v.resize(vsize as usize, 0);
				for &f in &skip {
					if f >= va && f + 4 <= va + vsize {
						v[(f - va) as usize..(f - va) as usize + 4].fill(0);
					}
				}
				(name, va, v)
			})
			.collect()
	}

	/// Realigns every PE image in $SALT_PE_DIR (e.g. a WebView2 install)
	/// and checks that each still loads to the same contents.
	#[test]
	#[ignore = "needs SALT_PE_DIR"]
	fn realign_preserves_sections() {
		let dir = std::env::var("SALT_PE_DIR").expect("SALT_PE_DIR");
		let mut n = 0;
		for e in std::fs::read_dir(dir).unwrap().flatten() {
			let p = e.path();
			let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
			if ext != "dll" && ext != "exe" {
				continue;
			}
			let d = std::fs::read(&p).unwrap();
			if layout(&d).is_err() {
				continue;
			}
			let new = realign(&d).unwrap();
			validate_bytes(&new).unwrap();
			let l = layout(&new).unwrap();
			for i in 0..l.nsections {
				assert_eq!(u32_at(&new, l.sections + 40 * i + 20).unwrap() % PAGE, 0, "{}", p.display());
			}
			assert_eq!(u32_at(&new, l.opt + 36).unwrap(), PAGE);
			for ((name, va, a), (_, vb, b)) in sections(&d).iter().zip(sections(&new).iter()) {
				let at = a.iter().zip(b).position(|(x, y)| x != y);
				assert!(va == vb && a.len() == b.len() && at.is_none(), "{} {name}: differs at {at:?}", p.display());
			}
			// Idempotent.
			let again = realign(&new).unwrap();
			if again != new {
				let at = again.iter().zip(&new).position(|(a, b)| a != b);
				panic!("{}: not idempotent, len {} vs {}, first diff at {at:?}", p.display(), again.len(), new.len());
			}
			n += 1;
		}
		assert!(n > 0);
	}
}
