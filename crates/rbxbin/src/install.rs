use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use md5::{Digest, Md5};

use crate::package::hex;
use crate::{Deployment, Error, Mirror, Package, PackageDirectories, Result};

/// Byte-weighted install progress: each package counts its compressed
/// size once downloaded (or verified from cache) and its uncompressed
/// size as it is extracted.
pub struct Progress<'a> {
	total: u64,
	done: AtomicU64,
	report: &'a (dyn Fn(f64) + Sync),
}

impl Progress<'_> {
	fn add(&self, n: u64) {
		let done = self.done.fetch_add(n, Ordering::Relaxed) + n;
		(self.report)(done as f64 / self.total.max(1) as f64);
	}

	/// Takes back bytes of a failed download attempt.
	fn sub(&self, n: u64) {
		self.done.fetch_sub(n, Ordering::Relaxed);
	}
}

fn agent() -> &'static ureq::Agent {
	static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
	AGENT.get_or_init(|| {
		ureq::Agent::config_builder()
			.timeout_connect(Some(Duration::from_secs(15)))
			.timeout_recv_response(Some(Duration::from_secs(30)))
			.http_status_as_error(false)
			.user_agent("rbxbin/v0.0.0")
			.build()
			.new_agent()
	})
}

/// Installs packages of a deployment into `dest`, caching downloads in
/// `downloads` by checksum.
///
/// Up to `jobs` packages are processed concurrently, largest first so a
/// big package never starts last and stretches the tail. Each download
/// is hashed while it streams to disk. On failure, remaining work stops
/// and the first error is returned.
#[allow(clippy::too_many_arguments)]
pub fn install(
	d: &Deployment,
	mirror: &Mirror,
	mut pkgs: Vec<Package>,
	dirs: &PackageDirectories,
	downloads: &Path,
	dest: &Path,
	jobs: usize,
	report: &(dyn Fn(f64) + Sync),
	cancel: &AtomicBool,
) -> Result<()> {
	// The installer is not a package of the installation.
	pkgs.retain(|p| {
		let skip = p.name.ends_with("Installer.exe");
		if skip {
			log::warn!("Skipping package {}", p.name);
		}
		!skip
	});
	for p in &pkgs {
		if !dirs.contains_key(&p.name) {
			return Err(Error::UnhandledPackage(p.name.clone()));
		}
	}
	pkgs.sort_by_key(|p| std::cmp::Reverse(p.zip_size));

	fs::create_dir_all(downloads)?;
	let progress = Progress {
		total: pkgs.iter().map(|p| p.zip_size + p.size).sum(),
		done: AtomicU64::new(0),
		report,
	};
	let queue = Mutex::new(VecDeque::from(pkgs));
	let failure: Mutex<Option<Error>> = Mutex::new(None);

	std::thread::scope(|s| {
		for _ in 0..jobs.max(1) {
			s.spawn(|| {
				loop {
					if cancel.load(Ordering::Relaxed) {
						return;
					}
					let Some(pkg) = queue.lock().unwrap().pop_front() else {
						return;
					};
					if let Err(e) = install_package(d, mirror, &pkg, dirs, downloads, dest, &progress, cancel) {
						cancel.store(true, Ordering::Relaxed);
						failure.lock().unwrap().get_or_insert(e);
						return;
					}
				}
			});
		}
	});

	match failure.into_inner().unwrap() {
		Some(e) => Err(e),
		None if cancel.load(Ordering::Relaxed) => Err(Error::Cancelled),
		None => Ok(()),
	}
}

#[allow(clippy::too_many_arguments)]
fn install_package(
	d: &Deployment,
	mirror: &Mirror,
	pkg: &Package,
	dirs: &PackageDirectories,
	downloads: &Path,
	dest: &Path,
	progress: &Progress,
	cancel: &AtomicBool,
) -> Result<()> {
	let src = downloads.join(&pkg.checksum);
	let cached = src.exists() && pkg.verify(&src).is_ok();
	if cached {
		progress.add(pkg.zip_size);
	} else {
		log::info!("Downloading package {} sum={}", pkg.name, pkg.checksum);
		download(&mirror.package_url(d, &pkg.name), &src, pkg, progress, cancel)?;
	}

	let dir = dest.join(&dirs[&pkg.name]);
	log::info!("Extracting package {} to {}", pkg.name, dir.display());
	pkg.extract(&src, &dir, &|n| progress.add(n))?;
	Ok(())
}

/// Downloads with up to three attempts, verifying the checksum as the
/// data streams in. HTTP status errors are not retried.
fn download(url: &str, dst: &Path, pkg: &Package, progress: &Progress, cancel: &AtomicBool) -> Result<()> {
	const ATTEMPTS: usize = 3;
	let mut last = None;
	for attempt in 1..=ATTEMPTS {
		let mut counted = 0u64;
		let r = try_download(url, dst, pkg, cancel, &mut |n| {
			counted += n;
			progress.add(n);
		});
		if r.is_err() {
			progress.sub(counted);
		}
		match r {
			Ok(()) => {
				// Content-Length may differ from the manifest's size.
				progress.add(pkg.zip_size.saturating_sub(counted));
				return Ok(());
			}
			Err(e @ (Error::Status { .. } | Error::Cancelled | Error::Io(_))) => {
				let _ = fs::remove_file(dst);
				return Err(e);
			}
			Err(e) => {
				let _ = fs::remove_file(dst);
				log::warn!("Download {url} failed (attempt {attempt}/{ATTEMPTS}): {e}");
				last = Some(e);
			}
		}
	}
	Err(last.expect("at least one attempt"))
}

fn try_download(url: &str, dst: &Path, pkg: &Package, cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64)) -> Result<()> {
	let mut resp = agent().get(url).call()?;
	let status = resp.status().as_u16();
	if status != 200 {
		return Err(Error::Status {
			url: url.into(),
			status,
		});
	}
	let tmp = dst.with_extension("part");
	let mut out = fs::File::create(&tmp)?;
	let mut body = resp.body_mut().as_reader();
	let mut h = Md5::new();
	let mut buf = vec![0u8; 256 * 1024];
	loop {
		if cancel.load(Ordering::Relaxed) {
			drop(out);
			let _ = fs::remove_file(&tmp);
			return Err(Error::Cancelled);
		}
		// Network errors surface as io::Error from the body reader;
		// classify them as retryable HTTP errors, not local I/O ones.
		let n = body
			.read(&mut buf)
			.map_err(|e| Error::Http(Box::new(ureq::Error::Io(e))))?;
		if n == 0 {
			break;
		}
		h.update(&buf[..n]);
		out.write_all(&buf[..n])?;
		on_bytes(n as u64);
	}
	out.sync_data()?;
	drop(out);

	let got = hex(&h.finalize());
	if got != pkg.checksum {
		let _ = fs::remove_file(&tmp);
		return Err(Error::Checksum {
			name: pkg.name.clone(),
			expected: pkg.checksum.clone(),
			got,
		});
	}
	fs::rename(&tmp, dst)?;
	Ok(())
}
