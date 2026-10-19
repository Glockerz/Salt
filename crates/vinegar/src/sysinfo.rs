//! Basic information about the host machine.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// A DRM GPU.
#[derive(Debug, Clone)]
pub struct Card {
	pub index: usize,
	/// Path to the PCI device.
	pub device: PathBuf,
	pub driver: String,
	/// Drives an integrated display (eDP/LVDS).
	pub embedded: bool,
	pub vendor_id: String,
	pub product_id: String,
	pub vendor: String,
	pub product: String,
}

impl Card {
	/// PCI address, used to identify the card in the configuration.
	pub fn addr(&self) -> String {
		self.device
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_default()
	}
}

impl std::fmt::Display for Card {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{} {}", self.vendor, self.product)
	}
}

pub struct SysInfo {
	pub cpu: String,
	/// Total memory in bytes.
	pub memory: u64,
	pub cards: Vec<Card>,
	pub distro: String,
	pub display: String,
	pub flatpak: bool,
	pub libc: String,
	/// Whether /dev/ntsync is usable, which lets Wine do thread
	/// synchronization in the kernel instead of round-tripping through
	/// wineserver.
	pub ntsync: bool,
}

pub static INFO: LazyLock<SysInfo> = LazyLock::new(|| {
	let flatpak = Path::new("/.flatpak-info").exists();
	let mut cards = cards();
	pci_names(&mut cards);
	SysInfo {
		cpu: cpu_name(),
		memory: total_memory(),
		cards,
		distro: distro(flatpak),
		display: format!(
			"{} ({})",
			std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
			std::env::var("XDG_SESSION_TYPE").unwrap_or_default()
		),
		flatpak,
		libc: interpreter().unwrap_or_default(),
		ntsync: fs::OpenOptions::new().read(true).write(true).open("/dev/ntsync").is_ok(),
	}
});

fn cpu_name() -> String {
	fs::read_to_string("/proc/cpuinfo")
		.ok()
		.and_then(|s| {
			s.lines()
				.find(|l| l.starts_with("model name"))
				.and_then(|l| l.split_once(':'))
				.map(|(_, v)| v.trim().to_owned())
		})
		.unwrap_or_default()
}

fn total_memory() -> u64 {
	fs::read_to_string("/proc/meminfo")
		.ok()
		.and_then(|s| {
			let l = s.lines().find(|l| l.starts_with("MemTotal:"))?;
			let kb: u64 = l["MemTotal:".len()..].trim().trim_end_matches(" kB").trim().parse().ok()?;
			Some(kb * 1024)
		})
		.unwrap_or(64)
}

fn distro(flatpak: bool) -> String {
	let path = if flatpak { "/run/host/os-release" } else { "/etc/os-release" };
	let Ok(s) = fs::read_to_string(path) else {
		return "Linux".into();
	};
	let mut name = "Linux".to_owned();
	for l in s.lines() {
		let Some((k, v)) = l.split_once('=') else { continue };
		let v = v.trim_matches('"');
		match k {
			"PRETTY_NAME" => name = v.to_owned(),
			"VERSION_ID" => {
				name.push(' ');
				name.push_str(v);
			}
			_ => {}
		}
	}
	name
}

const DRM: &str = "/sys/class/drm";

fn cards() -> Vec<Card> {
	let Ok(rd) = fs::read_dir(DRM) else {
		return Vec::new();
	};
	let mut names: Vec<String> = rd
		.filter_map(|e| e.ok())
		.map(|e| e.file_name().to_string_lossy().into_owned())
		.filter(|n| n.len() == 5 && n.starts_with("card") && n.as_bytes()[4].is_ascii_digit())
		.collect();
	names.sort();
	let entries: Vec<String> = fs::read_dir(DRM)
		.map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
		.unwrap_or_default();

	names
		.iter()
		.enumerate()
		.map(|(i, name)| {
			let path = Path::new(DRM).join(name);
			let dev = fs::canonicalize(path.join("device")).unwrap_or_default();
			let driver = fs::canonicalize(dev.join("driver"))
				.ok()
				.and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
				.unwrap_or_default();
			let read_id = |f: &str| {
				fs::read_to_string(dev.join(f))
					.map(|s| s.trim().trim_start_matches("0x").to_owned())
					.unwrap_or_default()
			};
			let prefix = format!("{name}-");
			Card {
				index: i,
				embedded: entries
					.iter()
					.any(|e| e.starts_with(&prefix) && (e.contains("eDP") || e.contains("LVDS"))),
				vendor_id: read_id("vendor"),
				product_id: read_id("device"),
				vendor: "unknown".into(),
				product: "unknown".into(),
				device: dev,
				driver,
			}
		})
		.collect()
}

/// Looks up card names in the PCI ID database. Only the needed entries
/// are searched for, instead of loading the whole database into memory.
fn pci_names(cards: &mut [Card]) {
	if cards.is_empty() {
		return;
	}
	let db = [
		"/usr/share/hwdata/pci.ids",
		"/usr/share/misc/pci.ids",
		"/usr/share/pci.ids",
		"/run/host/usr/share/hwdata/pci.ids",
	]
	.iter()
	.find_map(|p| fs::read_to_string(p).ok());
	let Some(db) = db else {
		log::debug!("PCI ID database not found");
		return;
	};
	let mut vendor = "";
	for line in db.lines() {
		if line.starts_with('#') || line.is_empty() {
			continue;
		}
		if !line.starts_with('\t') {
			vendor = line.get(..4).unwrap_or("");
			if let Some(name) = line.get(4..).map(str::trim) {
				for c in cards.iter_mut().filter(|c| c.vendor_id == vendor) {
					c.vendor = name.to_owned();
				}
			}
			continue;
		}
		if line.starts_with("\t\t") {
			continue;
		}
		let id = line.get(1..5).unwrap_or("");
		for c in cards.iter_mut().filter(|c| c.vendor_id == vendor && c.product_id == id) {
			c.product = line[5..].trim().to_owned();
		}
	}
}

/// The ELF interpreter of this executable, which names the libc.
fn interpreter() -> Option<String> {
	use object::read::elf::{ElfFile64, ProgramHeader};
	let data = fs::read("/proc/self/exe").ok()?;
	let elf = ElfFile64::<object::Endianness>::parse(&*data).ok()?;
	let endian = elf.endian();
	for ph in elf.elf_program_headers() {
		if ph.p_type(endian) == object::elf::PT_INTERP {
			let b = ph.data(endian, &*data).ok()?;
			return Some(String::from_utf8_lossy(b).trim_end_matches('\0').to_owned());
		}
	}
	None
}
