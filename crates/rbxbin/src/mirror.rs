use std::sync::mpsc;
use std::time::Duration;

use crate::package::{Package, PackageDirectories, parse_packages, scan_package_directories};
use crate::{Deployment, Error, Result};

/// A Roblox deployment mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror(pub String);

pub const DEFAULT_MIRROR: &str = "https://setup.rbxcdn.com";

/// Mirrors in order of preference. Mirrors may go down or be blocked by ISPs.
pub const MIRRORS: &[&str] = &[
	DEFAULT_MIRROR,
	"https://roblox-setup.cachefly.net",
	"https://s3.amazonaws.com/setup.roblox.com",
];

impl Mirror {
	/// Returns the most preferred reachable mirror.
	///
	/// All mirrors are probed at once, so an unreachable preferred mirror
	/// costs one timeout instead of one per mirror.
	pub fn get() -> Result<Mirror> {
		let agent: ureq::Agent = ureq::Agent::config_builder()
			.timeout_global(Some(Duration::from_secs(10)))
			.http_status_as_error(false)
			.build()
			.new_agent();
		let (tx, rx) = mpsc::channel();
		for (i, m) in MIRRORS.iter().enumerate() {
			let (tx, agent) = (tx.clone(), agent.clone());
			std::thread::spawn(move || {
				let ok = agent
					.head(format!("{m}/version"))
					.call()
					.is_ok_and(|r| r.status() == 200);
				let _ = tx.send((i, ok));
			});
		}
		drop(tx);

		let mut results = vec![None; MIRRORS.len()];
		for (i, ok) in rx {
			results[i] = Some(ok);
			// Done once every more-preferred mirror has answered.
			if let Some(best) = results.iter().position(|r| *r != Some(false)) {
				if results[best] == Some(true) {
					return Ok(Mirror(MIRRORS[best].into()));
				}
			} else {
				break;
			}
		}
		Err(Error::NoMirror)
	}

	/// URL of a package of a deployment. "common" serves every channel.
	pub fn package_url(&self, d: &Deployment, pkg: &str) -> String {
		format!("{}/channel/common/{}-{pkg}", self.0, d.guid)
	}

	pub fn packages(&self, d: &Deployment) -> Result<Vec<Package>> {
		let body = d.get(&self.package_url(d, "rbxPkgManifest.txt"))?;
		parse_packages(&String::from_utf8_lossy(&body))
	}

	/// Package destination directories, scanned from the deployment's installer.
	pub fn binary_directories(&self, d: &Deployment) -> Result<PackageDirectories> {
		let exe = d.get(&self.package_url(d, &format!("Roblox{}Installer.exe", d.kind.short())))?;
		let rdata = pe_section(&exe, ".rdata")?;
		scan_package_directories(&rdata).ok_or(Error::DirMapScan)
	}
}

fn pe_section(data: &[u8], name: &str) -> Result<Vec<u8>> {
	use object::read::pe::{PeFile32, PeFile64};
	use object::{Object, ObjectSection};
	fn find<'a>(f: &impl Object<'a>, name: &str) -> Option<Vec<u8>> {
		f.section_by_name(name)?.data().ok().map(<[u8]>::to_vec)
	}
	let found = match PeFile64::parse(data) {
		Ok(f) => find(&f, name),
		Err(_) => find(&PeFile32::parse(data).map_err(|e| Error::Pe(e.to_string()))?, name),
	};
	found.ok_or_else(|| Error::Pe(format!("missing section {name}")))
}
