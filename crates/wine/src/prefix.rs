use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::cmd::{Cmd, LineSink};
use crate::registry::Registry;
use crate::{Error, Result, boot, server};

/// A Wineprefix, which is where Wine stores its data and is
/// equivalent to a C:\ drive.
#[derive(Clone)]
pub struct Prefix {
	/// Path to a Wine or Proton installation. Empty means Wine is
	/// looked up in `PATH`.
	pub root: PathBuf,

	/// Additional environment variables for all descendant commands,
	/// applied on top of the inherited environment.
	pub env: Vec<(String, String)>,

	/// Receives every line Wine writes to stdout/stderr. When unset,
	/// output is inherited from the current process.
	pub sink: Option<LineSink>,

	dir: PathBuf,
}

impl std::fmt::Debug for Prefix {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Prefix")
			.field("dir", &self.dir)
			.field("root", &self.root)
			.finish_non_exhaustive()
	}
}

impl Prefix {
	/// Creates a new Prefix handle. The directory must be an absolute path
	/// owned by the current user.
	pub fn new(dir: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
		Prefix {
			root: root.into(),
			env: Vec::new(),
			sink: None,
			dir: dir.into(),
		}
	}

	pub fn dir(&self) -> &Path {
		&self.dir
	}

	/// Reports whether the root is a Proton installation.
	pub fn is_proton(&self) -> bool {
		!self.root.as_os_str().is_empty() && self.root.join("proton").exists()
	}

	/// Reports whether the prefix exists and has been set up.
	pub fn exists(&self) -> bool {
		self.dir.join("drive_c/windows").exists()
	}

	/// Path of the directory wineserver uses for this prefix's socket and lock.
	pub fn server_dir(&self) -> Option<PathBuf> {
		let md = fs::metadata(&self.dir).ok()?;
		let tmp = std::env::var_os("TMPDIR")
			.filter(|t| !t.is_empty())
			.map(PathBuf::from)
			.unwrap_or_else(|| PathBuf::from("/tmp"));
		// SAFETY: getuid cannot fail.
		let uid = unsafe { libc::getuid() };
		Some(tmp.join(format!(".wine-{uid}/server-{:x}-{:x}", md.dev(), md.ino())))
	}

	/// Reports whether a wineserver is running for this prefix.
	///
	/// The upstream implementation only checks that the socket exists,
	/// which reports a crashed server as running forever and makes the
	/// launcher skip preparing the prefix. wineserver holds a POSIX lock
	/// on `lock` for its whole lifetime, so query that lock instead.
	pub fn running(&self) -> bool {
		let Some(dir) = self.server_dir() else {
			return false;
		};
		match fs::File::open(dir.join("lock")) {
			Ok(f) => {
				// SAFETY: flock is plain old data; F_GETLK only writes into it.
				let mut fl: libc::flock = unsafe { std::mem::zeroed() };
				fl.l_type = libc::F_WRLCK as _;
				fl.l_whence = libc::SEEK_SET as _;
				let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &mut fl) };
				if r == 0 {
					return fl.l_type as libc::c_int != libc::F_UNLCK;
				}
				dir.join("socket").exists()
			}
			Err(_) => dir.join("socket").exists(),
		}
	}

	pub(crate) fn bin(&self, prog: &str) -> PathBuf {
		if self.is_proton() {
			self.root.join("files/bin").join(prog)
		} else if !self.root.as_os_str().is_empty() {
			self.root.join("bin").join(prog)
		} else {
			PathBuf::from(prog)
		}
	}

	/// Resolves the Wine executable, preferring `wine64` when it exists.
	pub fn wine_path(&self) -> Result<PathBuf> {
		if let Ok(p) = lookup(&self.bin("wine64")) {
			return Ok(p);
		}
		lookup(&self.bin("wine"))
	}

	/// Returns a command running `exe` under Wine.
	pub fn wine<S: AsRef<OsStr>>(&self, exe: impl AsRef<OsStr>, args: impl IntoIterator<Item = S>) -> Cmd {
		let wine = self.wine_path().unwrap_or_else(|_| self.bin("wine"));
		let mut c = self.command(wine);
		c.arg(exe);
		c.args(args);
		c
	}

	/// Returns a command for an arbitrary program within the prefix.
	pub fn command(&self, program: impl AsRef<OsStr>) -> Cmd {
		Cmd::new(self.clone(), program.as_ref())
	}

	/// Returns the prefix's Wine version, or "unknown".
	pub fn version(&self) -> String {
		match self.wine("--version", [""; 0]).quiet().output() {
			Ok((out, _)) if !out.is_empty() => String::from_utf8_lossy(&out).trim_end().to_owned(),
			_ => "unknown".into(),
		}
	}

	/// Runs wineserver with the given arguments. Exit codes 1 (already
	/// killed) and 2 (already started) are not errors.
	///
	/// Some builds (Kombucha 11.x) crash when wineserver is run directly
	/// rather than started by Wine itself. The Go version ignored this by
	/// accident (a signal exit reports code -1, which passed its `< 3`
	/// check); here it is ignored deliberately and logged.
	pub fn server(&self, args: &[&str]) -> Result<()> {
		match self.command(self.bin("wineserver")).args(args).quiet().run() {
			Err(Error::Exit { status, .. }) if status.code().is_some_and(|c| c < 3) => Ok(()),
			Err(Error::Exit { status, .. }) if status.code().is_none() => {
				log::debug!("wineserver {args:?} terminated abnormally: {status}");
				Ok(())
			}
			r => r,
		}
	}

	/// Returns a command for wineboot.
	pub fn boot(&self, args: &[&str]) -> Cmd {
		self.wine("wineboot", args)
	}

	/// Reports whether the prefix needs a full re-initialization for the
	/// current Wine installation.
	pub fn needs_update(&self) -> Result<bool> {
		let Ok(prefix_update) = self.updated() else {
			// Inaccessible or missing: let wineboot deal with it.
			return Ok(true);
		};
		let Some(prefix_update) = prefix_update else {
			return Ok(false); // disabled
		};
		let wine = self.wine_path()?;
		let inf = wine.parent().unwrap_or(Path::new("/")).join("../share/wine/wine.inf");
		let stamp = fs::metadata(inf)?.mtime();
		// programs/wineboot/wineboot.c:update_timestamp
		Ok(prefix_update != stamp)
	}

	fn updated(&self) -> Result<Option<i64>> {
		let b = fs::read_to_string(self.dir.join(".update-timestamp"))?;
		let content = b.trim();
		if content.starts_with("disable") {
			return Ok(None);
		}
		content
			.parse()
			.map(Some)
			.map_err(|e| Error::Other(format!("update timestamp: {e}")))
	}

	/// Ensures the prefix is up to date with the Wine installation,
	/// without showing Wine's update dialog.
	///
	/// This doesn't boot the prefix: the wineserver exits as soon as its
	/// last program does, so a boot only helps while a [`Hold`] keeps it.
	pub fn prepare(&self) -> Result<()> {
		match self.needs_update() {
			Err(e) => log::warn!("wine: Failed to determine Prefix status: {e}"),
			Ok(true) => {
				log::info!("wine: Updating Wine Prefix");
				return self.update();
			}
			Ok(false) => {}
		}
		Ok(())
	}

	/// Boots the prefix in the background and keeps its wineserver
	/// running until the returned [`Hold`] is dropped.
	pub fn hold(&self) -> Result<Hold> {
		// An idle cmd waiting on a pipe that only closes on drop.
		let mut cmd = self.wine("cmd", ["/c", "set /p x="]);
		cmd.quiet();
		let mut c = cmd.build()?;
		c.stdin(std::process::Stdio::piped());
		Ok(Hold(Some(c.spawn()?)))
	}

	/// Ensures the wineserver is running and the prefix is booted. Does
	/// nothing if the wineserver is already running.
	///
	/// wineboot starts the wineserver the way every Wine program does,
	/// which works even where running wineserver directly does not.
	pub fn start(&self) -> Result<()> {
		if self.running() {
			return Ok(());
		}
		self.boot(&[boot::RESTART]).run()
	}

	/// Kills every process of the prefix.
	pub fn kill(&self) -> Result<()> {
		if !self.exists() || !self.running() {
			return Ok(());
		}
		if self.boot(&[boot::KILL]).quiet().run().is_err() {
			return Ok(());
		}
		self.server(&[server::KILL])
	}

	/// Initializes the prefix headlessly.
	pub fn init(&self) -> Result<()> {
		self.boot(&[boot::INIT]).headless().run()
	}

	/// Fully re-initializes the prefix data headlessly.
	pub fn update(&self) -> Result<()> {
		self.boot(&[boot::UPDATE]).headless().run()
	}

	/// The current user's AppData directory within the prefix.
	pub fn appdata_dir(&self) -> PathBuf {
		let user = std::env::var("USER").unwrap_or_else(|_| "steamuser".into());
		self.dir.join("drive_c/users").join(user).join("AppData")
	}

	/// Parses the prefix's offline registry files.
	///
	/// This reflects the state on disk, which wineserver flushes
	/// periodically and on exit.
	pub fn registry(&self) -> Result<Registry> {
		Registry::load(&self.dir)
	}
}

/// Keeps a prefix's wineserver running; see [`Prefix::hold`].
///
/// wineserver exits the moment its last client does, and starting it
/// again (with the boot every new wineserver runs) takes over a second.
/// Asking it to persist instead (`wineserver -p`) crashes Kombucha builds.
pub struct Hold(Option<std::process::Child>);

impl Drop for Hold {
	fn drop(&mut self) {
		let Some(mut child) = self.0.take() else {
			return;
		};
		// Closing stdin ends cmd; reap it without blocking the caller.
		drop(child.stdin.take());
		std::thread::spawn(move || {
			for _ in 0..50 {
				if !matches!(child.try_wait(), Ok(None)) {
					return;
				}
				std::thread::sleep(std::time::Duration::from_millis(100));
			}
			let _ = child.kill();
			let _ = child.wait();
		});
	}
}

/// Resolves a program like exec.LookPath: paths containing a slash must
/// exist and be executable, bare names are searched in `PATH`.
fn lookup(p: &Path) -> Result<PathBuf> {
	use std::os::unix::fs::PermissionsExt;
	let is_exec = |p: &Path| {
		fs::metadata(p)
			.map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
			.unwrap_or(false)
	};
	if p.components().count() > 1 {
		return if is_exec(p) {
			Ok(p.to_owned())
		} else {
			Err(Error::NotFound(p.display().to_string()))
		};
	}
	let path = std::env::var_os("PATH").unwrap_or_default();
	std::env::split_paths(&path)
		.map(|d| d.join(p))
		.find(|c| is_exec(c))
		.ok_or_else(|| Error::NotFound(p.display().to_string()))
}
