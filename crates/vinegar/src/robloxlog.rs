//! Handling of Roblox's own log output, and following it through the
//! log files Studio writes.
//!
//! Studio logs heavily (one session in testing wrote 248,000 lines). The
//! Go version read these lines from Wine's debug channel, which turns
//! every log call in every Studio thread into a synchronous write to
//! Wine's stderr pipe. Tailing the files Studio writes anyway keeps that
//! work off Studio's threads entirely.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// What the log handler needs from the application.
pub trait LogEvents: Send + Sync {
	fn debug(&self) -> bool;
	fn rpc(&self, line: &str);
	fn instance_started(&self);
	fn instance_exited(&self);
	fn browser_login_failed(&self);
}

/// Handles one Roblox log line:
/// `time,runtime,thread,flags[,level] [Channel] message`.
pub fn handle(ev: &dyn LogEvents, line: &str) {
	let line = line.trim_end_matches('\r');
	if line.contains("LoginDialog Error: Embedded Web Browser fail to load") {
		ev.browser_login_failed();
	} else if line.contains("launching new studio instance") {
		log::warn!("New studio instance ran!");
		ev.instance_started();
		return;
	} else if line.contains("About to exit the application") {
		ev.instance_exited();
		return;
	}

	let mut parts = line.splitn(4, ',');
	let (Some(_), Some(_), Some(_), Some(rest)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
		crate::logging::write(crate::logging::Level::Roblox, line);
		return;
	};

	let (level, msg) = match rest.find(" [") {
		Some(i) => {
			let level = rest[..i].split_once(',').map(|(_, l)| l).unwrap_or("Info");
			(level, &rest[i + 1..])
		}
		None => ("Info", rest),
	};

	if crate::studiorpc::relevant(msg) {
		ev.rpc(msg);
	}
	if level == "Info" && !ev.debug() {
		return;
	}
	crate::logging::write(crate::logging::Level::Roblox, msg);
}

/// Extracts the Roblox message from a Wine `warn:seh` debug line:
/// `0128:warn:seh:OutputDebugStringA "message\n"`.
pub fn from_seh(line: &str) -> Option<&str> {
	let rest = line.get(5..)?;
	let rest = rest.strip_prefix("warn:seh:OutputDebugStringA \"")?;
	let rest = rest.strip_suffix('"').unwrap_or(rest);
	Some(rest.strip_suffix("\\n").unwrap_or(rest))
}

struct Tail {
	file: File,
	pending: Vec<u8>,
}

/// Follows Studio log files created after the watcher started, in any
/// of `dirs`, until `stop` is set.
pub fn watch(dirs: Vec<PathBuf>, ev: Arc<dyn LogEvents>, stop: Arc<AtomicBool>) {
	let existing: HashSet<PathBuf> = dirs.iter().flat_map(|d| list(d)).collect();
	std::thread::Builder::new()
		.name("robloxlog".into())
		.spawn(move || {
			let mut tails: HashMap<PathBuf, Tail> = HashMap::new();
			let mut buf = vec![0u8; 256 * 1024];
			loop {
				let stopping = stop.load(Ordering::Relaxed);
				for p in dirs.iter().flat_map(|d| list(d)) {
					if existing.contains(&p) || tails.contains_key(&p) {
						continue;
					}
					if let Ok(file) = File::open(&p) {
						log::debug!("Following Roblox log {}", p.display());
						tails.insert(p, Tail { file, pending: Vec::new() });
					}
				}
				for t in tails.values_mut() {
					drain(t, &mut buf, &*ev);
				}
				if stopping {
					return;
				}
				std::thread::sleep(Duration::from_millis(200));
			}
		})
		.expect("spawn robloxlog");
}

fn drain(t: &mut Tail, buf: &mut [u8], ev: &dyn LogEvents) {
	loop {
		let n = match t.file.read(buf) {
			Ok(0) | Err(_) => return,
			Ok(n) => n,
		};
		t.pending.extend_from_slice(&buf[..n]);
		let mut start = 0;
		while let Some(i) = t.pending[start..].iter().position(|&b| b == b'\n') {
			let line = String::from_utf8_lossy(&t.pending[start..start + i]);
			if !line.is_empty() {
				handle(ev, &line);
			}
			start += i + 1;
		}
		t.pending.drain(..start);
		// A log that was truncated and rewritten starts over.
		if let (Ok(pos), Ok(md)) = (t.file.stream_position(), t.file.metadata())
			&& md.len() < pos
		{
			let _ = t.file.seek(SeekFrom::Start(0));
			t.pending.clear();
		}
	}
}

fn list(dir: &Path) -> Vec<PathBuf> {
	fs::read_dir(dir)
		.map(|rd| {
			rd.filter_map(|e| e.ok())
				.map(|e| e.path())
				.filter(|p| {
					p.file_name()
						.and_then(|n| n.to_str())
						.is_some_and(|n| n.contains("_Studio_") && n.ends_with(".log"))
				})
				.collect()
		})
		.unwrap_or_default()
}

#[cfg(test)]
mod tests {
	use std::sync::Mutex;

	use super::*;

	#[derive(Default)]
	struct Rec {
		rpc: Mutex<Vec<String>>,
		started: Mutex<u32>,
		exited: Mutex<u32>,
	}

	impl LogEvents for Rec {
		fn debug(&self) -> bool {
			false
		}
		fn rpc(&self, l: &str) {
			self.rpc.lock().unwrap().push(l.into());
		}
		fn instance_started(&self) {
			*self.started.lock().unwrap() += 1;
		}
		fn instance_exited(&self) {
			*self.exited.lock().unwrap() += 1;
		}
		fn browser_login_failed(&self) {}
	}

	#[test]
	fn parse_lines() {
		let r = Rec::default();
		handle(&r, "2026-10-18T06:06:36.323Z,13.3,01e4,6 [FLog::CloseDataModel] Setting place ID 1818\r");
		handle(&r, "2026-10-18T06:06:36.323Z,13.3,01e4,6,Info [FLog::RobloxDocManager] Setting current doc to type 0");
		handle(&r, "a,b,c");
		handle(&r, "x,y,z,6 launching new studio instance");
		handle(&r, "x,y,z,6 About to exit the application");
		let rpc = r.rpc.lock().unwrap();
		assert_eq!(rpc[0], "[FLog::CloseDataModel] Setting place ID 1818");
		assert_eq!(rpc[1], "[FLog::RobloxDocManager] Setting current doc to type 0");
		assert_eq!(*r.started.lock().unwrap(), 1);
		assert_eq!(*r.exited.lock().unwrap(), 1);
	}

	#[test]
	fn seh() {
		assert_eq!(
			from_seh(r#"0128:warn:seh:OutputDebugStringA "2026-1,2,3,6 [X] hi\n""#),
			Some("2026-1,2,3,6 [X] hi")
		);
		assert_eq!(from_seh("0128:fixme:seh:WerRegisterCustomMetadata"), None);
	}

	#[test]
	fn tail_files() {
		let dir = std::env::temp_dir().join(format!("vinegar-logtest-{}", std::process::id()));
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		fs::write(dir.join("old_Studio_1_last.log"), "a,b,c,6 launching new studio instance\n").unwrap();

		let r = Arc::new(Rec::default());
		let stop = Arc::new(AtomicBool::new(false));
		watch(vec![dir.clone()], r.clone(), stop.clone());
		std::thread::sleep(Duration::from_millis(50));

		let p = dir.join("0.743_Studio_2_last.log");
		fs::write(&p, "t,1,2,6 launching new studio instance\nt,1,2,6 [FLog::CloseDataModel] Setting place ID 5\npart").unwrap();
		std::thread::sleep(Duration::from_millis(500));
		{
			use std::io::Write;
			let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
			f.write_all(b"ial,1,2,6 About to exit the application\n").unwrap();
		}
		std::thread::sleep(Duration::from_millis(500));
		stop.store(true, Ordering::Relaxed);
		std::thread::sleep(Duration::from_millis(300));

		assert_eq!(*r.started.lock().unwrap(), 1, "old files are ignored");
		assert_eq!(*r.exited.lock().unwrap(), 1, "partial lines are joined");
		assert_eq!(r.rpc.lock().unwrap().len(), 1);
		let _ = fs::remove_dir_all(&dir);
	}
}
