//! Logging to stderr (colored) and a per-run file, with two extra levels
//! for Wine (WIN) and Roblox (RBX) output.
//!
//! Wine and Roblox output can reach thousands of lines per second during
//! a playtest. Lines are formatted once into a buffer and the file is
//! flushed at most every 250 ms (and immediately for warnings and
//! errors), keeping the reader that drains Wine's pipe fast.

use std::fs::{self, File};
use std::io::{BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use crate::dirs::DIRS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
	Debug,
	Info,
	Wine,
	Roblox,
	Warn,
	Error,
}

impl Level {
	fn tag(self) -> &'static str {
		match self {
			Level::Debug => "DBG",
			Level::Info => "INF",
			Level::Wine => "WIN",
			Level::Roblox => "RBX",
			Level::Warn => "WRN",
			Level::Error => "ERR",
		}
	}

	fn color(self) -> &'static str {
		match self {
			Level::Debug => "\x1b[2m",
			Level::Info => "\x1b[92m",
			Level::Wine => "\x1b[91m",
			Level::Roblox => "\x1b[96m",
			Level::Warn => "\x1b[93m",
			Level::Error => "\x1b[91;1m",
		}
	}
}

struct Sink {
	file: Option<BufWriter<File>>,
	last_flush: Option<Instant>,
}

static DEBUG: AtomicBool = AtomicBool::new(false);
static COLOR: LazyLock<bool> = LazyLock::new(|| std::io::stderr().is_terminal());
static SINK: Mutex<Sink> = Mutex::new(Sink {
	file: None,
	last_flush: None,
});

/// Path of this run's log file.
pub static PATH: LazyLock<PathBuf> = LazyLock::new(|| DIRS.logs.join(format!("{}.log", rfc3339_now())));

pub fn set_debug(on: bool) {
	DEBUG.store(on, Ordering::Relaxed);
	log::set_max_level(if on { log::LevelFilter::Debug } else { log::LevelFilter::Info });
}

pub fn debug_enabled() -> bool {
	DEBUG.load(Ordering::Relaxed)
}

fn enabled(level: Level) -> bool {
	level != Level::Debug || debug_enabled()
}

/// Writes a log record.
pub fn write(level: Level, msg: &str) {
	if !enabled(level) {
		return;
	}
	let time = clock();
	if *COLOR {
		eprintln!("\x1b[2m{time}\x1b[0m {}{}\x1b[0m {msg}", level.color(), level.tag());
	} else {
		eprintln!("{time} {} {msg}", level.tag());
	}
	let mut s = SINK.lock().unwrap_or_else(|e| e.into_inner());
	let now = Instant::now();
	let flush = level >= Level::Warn || s.last_flush.is_none_or(|t| now.duration_since(t).as_millis() >= 250);
	if let Some(f) = &mut s.file {
		let _ = writeln!(f, "{time} {} {msg}", level.tag());
		if flush {
			let _ = f.flush();
		}
	}
	if flush {
		s.last_flush = Some(now);
	}
}

pub fn flush() {
	if let Some(f) = &mut SINK.lock().unwrap_or_else(|e| e.into_inner()).file {
		let _ = f.flush();
	}
}

struct Logger;

impl log::Log for Logger {
	fn enabled(&self, m: &log::Metadata) -> bool {
		m.level() <= log::Level::Info || debug_enabled()
	}

	fn log(&self, r: &log::Record) {
		let level = match r.level() {
			log::Level::Error => Level::Error,
			log::Level::Warn => Level::Warn,
			log::Level::Info => Level::Info,
			_ => Level::Debug,
		};
		// Dependencies' debug chatter (TLS, HTTP internals) is noise.
		let ours = r
			.module_path()
			.is_some_and(|m| ["salt", "wine", "rbxbin", "rbxweb"].iter().any(|p| m.starts_with(p)));
		if !ours && level < Level::Warn {
			return;
		}
		write(level, &r.args().to_string());
	}

	fn flush(&self) {
		flush();
	}
}

/// Installs the logger without a log file, for helper commands whose
/// runs would otherwise rotate the launcher's logs away.
pub fn init_stderr_only() {
	let _ = log::set_logger(&Logger);
	set_debug(false);
}

/// Installs the logger, opens this run's log file and removes old logs.
pub fn init() {
	let _ = log::set_logger(&Logger);
	set_debug(false);

	match fs::create_dir_all(&DIRS.logs).and_then(|_| File::options().append(true).create(true).open(&*PATH)) {
		Ok(f) => {
			SINK.lock().unwrap().file = Some(BufWriter::with_capacity(64 * 1024, f));
			log::info!("Logging to file path={}", PATH.display());
		}
		Err(e) => log::error!("Failed to open log file: {e}"),
	}

	if let Err(e) = cleanup() {
		log::error!("Failed to clear old log files: {e}");
	}

	// Lines are only flushed when another line arrives; make sure the
	// tail of a burst reaches the file too.
	std::thread::Builder::new()
		.name("log-flush".into())
		.spawn(|| {
			loop {
				std::thread::sleep(std::time::Duration::from_secs(1));
				flush();
			}
		})
		.ok();
}

/// Keeps the 16 most recent logs that Salt created.
fn cleanup() -> std::io::Result<()> {
	let mut logs: Vec<_> = fs::read_dir(&DIRS.logs)?
		.filter_map(|e| e.ok())
		.map(|e| e.file_name().to_string_lossy().into_owned())
		.filter(|n| n.len() == 29 && n.ends_with(".log"))
		.collect();
	logs.sort();
	let excess = logs.len().saturating_sub(16);
	for name in &logs[..excess] {
		fs::remove_file(DIRS.logs.join(name))?;
	}
	Ok(())
}

fn local_offset_secs() -> i64 {
	// SAFETY: localtime_r only writes into the provided struct.
	unsafe {
		let t = libc::time(std::ptr::null_mut());
		let mut tm: libc::tm = std::mem::zeroed();
		libc::localtime_r(&t, &mut tm);
		tm.tm_gmtoff
	}
}

fn local_tm() -> libc::tm {
	// SAFETY: as above.
	unsafe {
		let t = libc::time(std::ptr::null_mut());
		let mut tm: libc::tm = std::mem::zeroed();
		libc::localtime_r(&t, &mut tm);
		tm
	}
}

/// HH:MM:SS local time.
fn clock() -> String {
	let tm = local_tm();
	format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// RFC 3339 local time, as in the Go version's log file names.
fn rfc3339_now() -> String {
	let tm = local_tm();
	let off = local_offset_secs();
	let (sign, off) = if off < 0 { ('-', -off) } else { ('+', off) };
	format!(
		"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
		tm.tm_year + 1900,
		tm.tm_mon + 1,
		tm.tm_mday,
		tm.tm_hour,
		tm.tm_min,
		tm.tm_sec,
		off / 3600,
		off % 3600 / 60
	)
}
