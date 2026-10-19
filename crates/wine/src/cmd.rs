use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{Child as StdChild, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::{Error, Prefix, Result, server};

/// Receives one line of Wine output, without the trailing newline.
pub type LineSink = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Size requested for output pipes. Wine writes debug output synchronously,
/// sometimes while holding loader or heap locks; a full pipe stalls the
/// writing thread, and with it every thread waiting on that lock. The
/// default 64 KiB fills in milliseconds when Studio logs heavily.
const PIPE_SIZE: libc::c_int = 1 << 20;

enum Output {
	Sink,
	Null,
	Inherit,
}

/// A command run within a Wineprefix.
///
/// Program and arguments are kept separately from [`Command`] so that a
/// launcher (such as gamemoderun) can be prepended after construction.
pub struct Cmd {
	prefix: Prefix,
	program: OsString,
	args: Vec<OsString>,
	envs: Vec<(OsString, OsString)>,
	output: Output,
	stdin: Option<Vec<u8>>,
	headless: bool,
}

impl fmt::Display for Cmd {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.program.to_string_lossy())?;
		for a in &self.args {
			write!(f, " {}", a.to_string_lossy())?;
		}
		Ok(())
	}
}

impl Cmd {
	pub(crate) fn new(prefix: Prefix, program: &OsStr) -> Self {
		let output = if prefix.sink.is_some() {
			Output::Sink
		} else {
			Output::Inherit
		};
		Cmd {
			prefix,
			program: program.to_owned(),
			args: Vec::new(),
			envs: Vec::new(),
			output,
			stdin: None,
			headless: false,
		}
	}

	pub fn arg(&mut self, a: impl AsRef<OsStr>) -> &mut Self {
		self.args.push(a.as_ref().to_owned());
		self
	}

	pub fn args<S: AsRef<OsStr>>(&mut self, args: impl IntoIterator<Item = S>) -> &mut Self {
		self.args.extend(args.into_iter().map(|a| a.as_ref().to_owned()));
		self
	}

	pub fn env(&mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> &mut Self {
		self.envs.push((k.as_ref().to_owned(), v.as_ref().to_owned()));
		self
	}

	/// Discards all output.
	pub fn quiet(&mut self) -> &mut Self {
		self.output = Output::Null;
		self
	}

	/// Runs the command without a display, used for wineboot. The
	/// wineserver is restarted afterwards so that later programs get one.
	pub fn headless(&mut self) -> &mut Self {
		self.headless = true;
		self
	}

	/// Feeds the given bytes to the command's standard input.
	pub fn stdin(&mut self, data: Vec<u8>) -> &mut Self {
		self.stdin = Some(data);
		self
	}

	pub fn program(&self) -> &OsStr {
		&self.program
	}

	pub fn get_args(&self) -> &[OsString] {
		&self.args
	}

	/// Replaces the program, moving the current program into the
	/// argument list after `pre_args`: `program pre_args... old args...`.
	pub fn wrap(&mut self, program: impl AsRef<OsStr>, pre_args: &[impl AsRef<OsStr>]) -> &mut Self {
		let old = std::mem::replace(&mut self.program, program.as_ref().to_owned());
		let mut args: Vec<OsString> = pre_args.iter().map(|a| a.as_ref().to_owned()).collect();
		args.push(old);
		args.append(&mut self.args);
		self.args = args;
		self
	}

	/// Inserts arguments right after the first argument (the Windows
	/// executable, for commands from [`Prefix::wine`]).
	pub fn insert_args(&mut self, at: usize, ins: &[impl AsRef<OsStr>]) -> &mut Self {
		let at = at.min(self.args.len());
		self.args
			.splice(at..at, ins.iter().map(|a| a.as_ref().to_owned()));
		self
	}

	pub(crate) fn build(&self) -> Result<Command> {
		let dir = self.prefix.dir();
		if !dir.as_os_str().is_empty() && !dir.is_absolute() {
			return Err(Error::PrefixNotAbs);
		}
		if Path::new(&self.program).components().count() > 1 && !Path::new(&self.program).exists() {
			return Err(Error::NotFound(self.program.to_string_lossy().into_owned()));
		}

		let mut c = Command::new(&self.program);
		c.args(&self.args);
		// Wine signals whole process groups when a prefix shuts down;
		// sharing one with the launcher would kill the launcher too.
		std::os::unix::process::CommandExt::process_group(&mut c, 0);
		for (k, v) in &self.prefix.env {
			c.env(k, v);
		}
		if !dir.as_os_str().is_empty() {
			c.env("WINEPREFIX", dir);
			// Wine complains if the prefix's parent doesn't exist.
			std::fs::create_dir_all(dir)?;
		}
		if self.headless {
			c.env("DISPLAY", "")
				.env("WAYLAND_DISPLAY", "")
				.env("WINEDEBUG", "fixme-all,-winediag,-systray,-ole,-ntoskrnl");
		}
		for (k, v) in &self.envs {
			c.env(k, v);
		}
		c.stdin(if self.stdin.is_some() {
			Stdio::piped()
		} else {
			Stdio::null()
		});
		match self.output {
			Output::Sink => {
				c.stdout(Stdio::piped()).stderr(Stdio::piped());
			}
			Output::Null => {
				c.stdout(Stdio::null()).stderr(Stdio::null());
			}
			Output::Inherit => {}
		}
		Ok(c)
	}

	/// Starts the command.
	pub fn spawn(&mut self) -> Result<Child> {
		log::debug!("Running Wine command: {self}");
		let mut child = self.build()?.spawn().map_err(|e| match e.kind() {
			std::io::ErrorKind::NotFound => Error::NotFound(self.program.to_string_lossy().into_owned()),
			_ => Error::Io(e),
		})?;

		if let Some(data) = self.stdin.take()
			&& let Some(mut stdin) = child.stdin.take()
		{
			thread::spawn(move || {
				let _ = stdin.write_all(&data);
			});
		}

		if let (Output::Sink, Some(sink)) = (&self.output, &self.prefix.sink) {
			if let Some(out) = child.stdout.take() {
				pipe_lines(out, sink.clone());
			}
			if let Some(err) = child.stderr.take() {
				pipe_lines(err, sink.clone());
			}
		}

		Ok(Child {
			inner: child,
			prefix: self.prefix.clone(),
			headless: self.headless,
		})
	}

	/// Runs the command to completion; a non-zero exit is an error.
	pub fn run(&mut self) -> Result<()> {
		let status = self.spawn()?.wait()?;
		if status.success() {
			Ok(())
		} else {
			Err(Error::Exit {
				program: self.program.to_string_lossy().into_owned(),
				status,
			})
		}
	}

	/// Runs the command and returns its standard output. Stderr goes to
	/// the sink, if any. The exit status is not checked; callers inspect
	/// the output, as `reg` reports errors on stdout.
	pub fn output(&mut self) -> Result<(Vec<u8>, ExitStatus)> {
		let mut c = self.build()?;
		c.stdout(Stdio::piped());
		if !matches!(self.output, Output::Sink) {
			c.stderr(Stdio::null());
		}
		log::debug!("Running Wine command: {self}");
		let mut child = c.spawn()?;
		if let Some(data) = self.stdin.take()
			&& let Some(mut stdin) = child.stdin.take()
		{
			thread::spawn(move || {
				let _ = stdin.write_all(&data);
			});
		}
		if let (Some(err), Some(sink)) = (child.stderr.take(), &self.prefix.sink) {
			pipe_lines(err, sink.clone());
		}

		// Wine daemons (wineserver, services) may inherit the pipe and
		// keep it open after the command itself exits, so never wait for
		// EOF unconditionally.
		let mut out = child.stdout.take().expect("piped");
		let (tx, rx) = mpsc::channel();
		thread::spawn(move || {
			let mut buf = Vec::new();
			let _ = out.read_to_end(&mut buf);
			let _ = tx.send(buf);
		});
		let status = child.wait()?;
		let buf = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
		Ok((buf, status))
	}
}

/// A running Wine command.
pub struct Child {
	pub inner: StdChild,
	prefix: Prefix,
	headless: bool,

}

impl Child {
	pub fn id(&self) -> u32 {
		self.inner.id()
	}

	pub fn kill(&mut self) -> std::io::Result<()> {
		self.inner.kill()
	}

	/// Waits for the command. Headless commands stop the wineserver
	/// afterwards, as it was started without a display; the next program
	/// starts a new one.
	pub fn wait(mut self) -> Result<ExitStatus> {
		let status = self.inner.wait()?;
		if self.headless && self.prefix.running() {
			let _ = self.prefix.server(&[server::KILL]);
		}
		Ok(status)
	}
}

fn pipe_lines(r: impl Read + AsRawFd + Send + 'static, sink: LineSink) {
	// SAFETY: fcntl on a valid pipe fd; failure is harmless.
	unsafe { libc::fcntl(r.as_raw_fd(), libc::F_SETPIPE_SZ, PIPE_SIZE) };
	thread::Builder::new()
		.name("wine-output".into())
		.spawn(move || {
			let mut r = BufReader::with_capacity(256 * 1024, r);
			let mut line = Vec::with_capacity(512);
			loop {
				line.clear();
				match r.read_until(b'\n', &mut line) {
					Ok(0) | Err(_) => break,
					Ok(_) => {
						if line.last() == Some(&b'\n') {
							line.pop();
						}
						if !line.is_empty() {
							sink(&line);
						}
					}
				}
			}
		})
		.expect("spawn output reader");
}
