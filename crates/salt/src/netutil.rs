//! Shared networking helpers.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub fn agent() -> &'static ureq::Agent {
	static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
	AGENT.get_or_init(|| {
		ureq::Agent::config_builder()
			.timeout_connect(Some(Duration::from_secs(15)))
			.timeout_recv_response(Some(Duration::from_secs(30)))
			.user_agent(concat!("salt/", env!("CARGO_PKG_VERSION")))
			.build()
			.new_agent()
	})
}

/// Downloads a URL to a file, reporting progress as a fraction. The data
/// goes to a temporary file that is renamed into place on success, so an
/// interrupted download never looks complete. Transient failures are
/// retried up to three times.
pub fn download(url: &str, file: &Path, progress: &dyn Fn(f64)) -> Result<()> {
	let mut last: Option<Error> = None;
	for attempt in 1..=3 {
		match try_download(url, file, progress) {
			Ok(()) => return Ok(()),
			Err(e) => {
				let status = e
					.downcast_ref::<ureq::Error>()
					.is_some_and(|e| matches!(e, ureq::Error::StatusCode(_)));
				if status || e.is::<std::io::Error>() {
					return Err(e);
				}
				log::warn!("Download {url} failed (attempt {attempt}/3): {e}");
				last = Some(e);
			}
		}
	}
	Err(last.unwrap())
}

fn try_download(url: &str, file: &Path, progress: &dyn Fn(f64)) -> Result<()> {
	let mut resp = agent().get(url).call()?;
	let total: Option<u64> = resp
		.headers()
		.get("content-length")
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.parse().ok());
	let tmp = file.with_extension("part");
	let mut out = fs::File::create(&tmp)?;
	let mut body = resp.body_mut().as_reader();
	let mut buf = vec![0u8; 256 * 1024];
	let mut done = 0u64;
	loop {
		let n = body.read(&mut buf).map_err(|e| Box::new(ureq::Error::Io(e)))?;
		if n == 0 {
			break;
		}
		out.write_all(&buf[..n])?;
		done += n as u64;
		if let Some(t) = total {
			progress(done as f64 / t.max(1) as f64);
		}
	}
	out.sync_data()?;
	fs::rename(tmp, file)?;
	Ok(())
}

/// Downloads and unpacks an .tar.xz tarball into `dir`, streaming.
///
/// The archive is unpacked into a scratch directory next to its final
/// location and moved into place at the end; the Go version unpacked in
/// place, so an interrupted download left a broken Wine build that was
/// then treated as installed.
pub fn extract_tar_xz(url: &str, dir: &Path) -> Result<Vec<PathBuf>> {
	let mut resp = agent().get(url).call()?;
	let body = resp.body_mut().as_reader();
	let xz = liblzma::read::XzDecoder::new_multi_decoder(std::io::BufReader::with_capacity(1 << 20, body));
	let mut ar = tar::Archive::new(xz);
	ar.set_preserve_permissions(true);
	ar.set_preserve_mtime(true);

	fs::create_dir_all(dir)?;
	let scratch = dir.join(".extract");
	let _ = fs::remove_dir_all(&scratch);
	fs::create_dir_all(&scratch)?;
	if let Err(e) = ar.unpack(&scratch) {
		let _ = fs::remove_dir_all(&scratch);
		return Err(e.into());
	}

	let mut moved = Vec::new();
	for e in fs::read_dir(&scratch)? {
		let e = e?;
		let dst = dir.join(e.file_name());
		if dst.exists() {
			fs::remove_dir_all(&dst).or_else(|_| fs::remove_file(&dst))?;
		}
		fs::rename(e.path(), &dst)?;
		moved.push(dst);
	}
	fs::remove_dir_all(&scratch)?;
	Ok(moved)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
	pub tag_name: String,
	#[serde(default)]
	pub published_at: Option<String>,
	#[serde(default)]
	pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
	pub content_type: String,
	pub browser_download_url: String,
}

fn github<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
	Ok(agent()
		.get(format!("https://api.github.com/repos/{path}"))
		.header("Accept", "application/vnd.github+json")
		.call()?
		.body_mut()
		.read_json()?)
}

/// A release of vinegarhq/kombucha: "Latest" or a tag.
pub fn kombucha_release(needle: &str) -> Result<Release> {
	if needle == "Latest" {
		github("vinegarhq/kombucha/releases/latest")
	} else {
		github(&format!("vinegarhq/kombucha/releases/tags/{needle}"))
	}
}

pub fn kombucha_releases() -> Result<Vec<Release>> {
	github("vinegarhq/kombucha/releases")
}
