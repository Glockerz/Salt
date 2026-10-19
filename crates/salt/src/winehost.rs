//! Runs StudioMCP.exe for `salt mcp` inside the Salt instance that
//! hosts Wine.
//!
//! Under Flatpak, every `flatpak run` gets its own PID namespace, while
//! all instances of an app share one wineserver per prefix. wineserver
//! signals client processes by the PID they reported from their own
//! namespace, so a Wine process started from another sandbox (as an MCP
//! server would be) makes wineserver signal unrelated processes in its
//! namespace: in testing, ending such a process killed Salt itself.
//!
//! So the MCP bridge never runs Wine. It connects to this socket and
//! passes the two pipe ends that become StudioMCP's stdin and stdout;
//! Salt starts StudioMCP in its own sandbox, next to Studio, and kills
//! it when the bridge disconnects.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::core::Core;
use crate::dirs::DIRS;

pub const PORT: u16 = 13469;
const REQUEST: &[u8] = b"studio-mcp\n";

/// Socket path, shared by all instances of the app.
pub fn socket_path() -> PathBuf {
	let run = std::env::var_os("XDG_RUNTIME_DIR")
		.map(PathBuf::from)
		.unwrap_or_else(std::env::temp_dir);
	match std::env::var("FLATPAK_ID") {
		Ok(id) => run.join("app").join(id).join("salt-wine.sock"),
		Err(_) => run.join("salt-wine.sock"),
	}
}

/// Sends `fds` with a payload over a Unix socket.
fn send_fds(sock: &UnixStream, payload: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
	// SAFETY: msghdr and the control buffer are fully initialized below,
	// and CMSG_* are used as documented for SCM_RIGHTS.
	unsafe {
		let mut iov = libc::iovec {
			iov_base: payload.as_ptr() as *mut _,
			iov_len: payload.len(),
		};
		let space = libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) as usize;
		let mut buf = vec![0u8; space];
		let mut msg: libc::msghdr = std::mem::zeroed();
		msg.msg_iov = &mut iov;
		msg.msg_iovlen = 1;
		msg.msg_control = buf.as_mut_ptr() as *mut _;
		msg.msg_controllen = space as _;
		let cmsg = libc::CMSG_FIRSTHDR(&msg);
		(*cmsg).cmsg_level = libc::SOL_SOCKET;
		(*cmsg).cmsg_type = libc::SCM_RIGHTS;
		(*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
		std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg) as *mut RawFd, fds.len());
		if libc::sendmsg(sock.as_raw_fd(), &msg, 0) < 0 {
			return Err(std::io::Error::last_os_error());
		}
	}
	Ok(())
}

/// Receives a payload and up to `max` fds.
fn recv_fds(sock: &UnixStream, max: usize) -> std::io::Result<(Vec<u8>, Vec<OwnedFd>)> {
	let mut data = vec![0u8; 256];
	let mut fds = Vec::new();
	// SAFETY: as in send_fds; received fds are wrapped immediately.
	unsafe {
		let mut iov = libc::iovec {
			iov_base: data.as_mut_ptr() as *mut _,
			iov_len: data.len(),
		};
		let space = libc::CMSG_SPACE((max * std::mem::size_of::<RawFd>()) as u32) as usize;
		let mut buf = vec![0u8; space];
		let mut msg: libc::msghdr = std::mem::zeroed();
		msg.msg_iov = &mut iov;
		msg.msg_iovlen = 1;
		msg.msg_control = buf.as_mut_ptr() as *mut _;
		msg.msg_controllen = space as _;
		let n = libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
		if n < 0 {
			return Err(std::io::Error::last_os_error());
		}
		data.truncate(n as usize);
		let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
		while !cmsg.is_null() {
			if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
				let len = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
				let p = libc::CMSG_DATA(cmsg) as *const RawFd;
				for i in 0..len / std::mem::size_of::<RawFd>() {
					fds.push(OwnedFd::from_raw_fd(*p.add(i)));
				}
			}
			cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
		}
	}
	Ok((data, fds))
}

/// Reports whether something listens on the StudioMCP port.
pub fn port_in_use() -> bool {
	let needle = format!(":{PORT:04X}");
	["/proc/net/tcp", "/proc/net/tcp6"].iter().any(|f| {
		std::fs::read_to_string(f).is_ok_and(|s| {
			s.lines().skip(1).any(|l| {
				let cols: Vec<&str> = l.split_whitespace().collect();
				cols.len() > 3 && cols[1].ends_with(&needle) && cols[3] == "0A"
			})
		})
	})
}

/// The newest installed Studio's StudioMCP.exe.
pub fn studio_mcp(cfg: &Config) -> Option<PathBuf> {
	if !cfg.studio.forced_version.is_empty() {
		let p = DIRS.versions.join(&cfg.studio.forced_version).join("StudioMCP.exe");
		return p.exists().then_some(p);
	}
	std::fs::read_dir(&DIRS.versions)
		.ok()?
		.flatten()
		.map(|e| e.path().join("StudioMCP.exe"))
		.filter(|p| p.exists())
		.max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

/// Starts serving MCP spawn requests for the lifetime of the process.
pub fn serve(core: Arc<Core>) {
	let path = socket_path();
	if let Some(dir) = path.parent() {
		let _ = std::fs::create_dir_all(dir);
	}
	let listener = match UnixListener::bind(&path) {
		Ok(l) => l,
		Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
			if UnixStream::connect(&path).is_ok() {
				log::warn!("Another Salt instance serves {}", path.display());
				return;
			}
			let _ = std::fs::remove_file(&path);
			match UnixListener::bind(&path) {
				Ok(l) => l,
				Err(e) => return log::error!("MCP socket: {e}"),
			}
		}
		Err(e) => return log::error!("MCP socket: {e}"),
	};
	log::debug!("Serving MCP requests on {}", path.display());
	std::thread::Builder::new()
		.name("winehost".into())
		.spawn(move || {
			for conn in listener.incoming().flatten() {
				let core = core.clone();
				std::thread::spawn(move || {
					if let Err(e) = handle(&core, conn) {
						log::warn!("MCP request failed: {e}");
					}
				});
			}
		})
		.expect("spawn winehost");
}

fn handle(core: &Core, conn: UnixStream) -> std::io::Result<()> {
	let (payload, mut fds) = recv_fds(&conn, 2)?;
	let reply = |s: &str| (&conn).write_all(s.as_bytes());
	if payload != REQUEST || fds.len() != 2 {
		return reply("ERR bad request\n");
	}
	let pfx = core.pfx();
	if !pfx.running() || core.count.load(std::sync::atomic::Ordering::Relaxed) == 0 {
		return reply("ERR studio not running\n");
	}
	let Some(exe) = studio_mcp(&core.cfg()) else {
		return reply("ERR StudioMCP.exe not found\n");
	};
	let wine = match pfx.wine_path() {
		Ok(w) => w,
		Err(e) => return reply(&format!("ERR {e}\n")),
	};

	if port_in_use() {
		log::warn!("Port {PORT} is in use; stopping stale StudioMCP");
		let _ = pfx.wine("taskkill", ["/f", "/im", "StudioMCP.exe"]).quiet().run();
		let deadline = Instant::now() + Duration::from_secs(5);
		while port_in_use() && Instant::now() < deadline {
			std::thread::sleep(Duration::from_millis(100));
		}
	}

	let stdout = fds.pop().expect("two fds");
	let stdin = fds.pop().expect("two fds");
	let mut cmd = Command::new(wine);
	cmd.arg(&exe).arg("--stdio");
	for (k, v) in &pfx.env {
		cmd.env(k, v);
	}
	cmd.env("WINEPREFIX", pfx.dir())
		.env("WINEDEBUG", "-all")
		.stdin(Stdio::from(stdin))
		.stdout(Stdio::from(stdout))
		.stderr(Stdio::null())
		.process_group(0);
	// If Salt dies, nothing would stop StudioMCP otherwise; this thread
	// lives exactly as long as it.
	// SAFETY: prctl is async-signal-safe.
	unsafe {
		cmd.pre_exec(|| {
			libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
			Ok(())
		});
	}
	let mut child = match cmd.spawn() {
		Ok(c) => c,
		Err(e) => return reply(&format!("ERR {e}\n")),
	};
	log::info!("Started StudioMCP for an MCP client pid={}", child.id());
	reply("OK\n")?;

	// Kill StudioMCP when the bridge goes away; report when it exits.
	conn.set_read_timeout(Some(Duration::from_millis(500)))?;
	let mut buf = [0u8; 64];
	loop {
		if let Some(status) = child.try_wait()? {
			log::info!("StudioMCP exited: {status}");
			let _ = (&conn).write_all(b"EXIT\n");
			return Ok(());
		}
		match (&conn).read(&mut buf) {
			Ok(0) => {
				log::info!("MCP client disconnected; stopping StudioMCP");
				let _ = child.kill();
				let _ = child.wait();
				return Ok(());
			}
			Ok(_) => {}
			Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
			Err(e) => {
				let _ = child.kill();
				let _ = child.wait();
				return Err(e);
			}
		}
	}
}

/// A StudioMCP process run by the hosting Salt instance.
pub struct Remote {
	pub stdin: std::fs::File,
	pub stdout: std::fs::File,
	/// Held open for StudioMCP's lifetime; closing it stops StudioMCP.
	_conn: UnixStream,
}

impl Remote {
	/// Returns a function that blocks until StudioMCP exits or the
	/// hosting Salt goes away.
	///
	/// StudioMCP's stdout alone can't tell: if the host dies, StudioMCP
	/// may outlive it, holding the pipe open without ever answering.
	pub fn host_gone(&self) -> std::io::Result<impl FnOnce() + Send + 'static> {
		let conn = self._conn.try_clone()?;
		Ok(move || {
			let mut line = String::new();
			let _ = BufReader::new(conn).read_line(&mut line);
		})
	}
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
	let mut fds = [0; 2];
	// SAFETY: pipe2 fills both fds on success.
	if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
		return Err(std::io::Error::last_os_error());
	}
	// SAFETY: freshly created and owned here.
	Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Asks the hosting Salt instance to start StudioMCP. `Ok(None)` means
/// Studio isn't running yet.
pub fn request() -> std::io::Result<Option<Remote>> {
	let conn = match UnixStream::connect(socket_path()) {
		Ok(c) => c,
		Err(_) => return Ok(None),
	};
	let (in_r, in_w) = pipe()?;
	let (out_r, out_w) = pipe()?;
	send_fds(&conn, REQUEST, &[in_r.as_raw_fd(), out_w.as_raw_fd()])?;
	// The child's ends now live in the host; close ours so EOF works.
	drop((in_r, out_w));
	conn.set_read_timeout(Some(Duration::from_secs(30)))?;
	let mut line = String::new();
	BufReader::new(&conn).read_line(&mut line)?;
	conn.set_read_timeout(None)?;
	if line.trim() != "OK" {
		log::debug!("Salt: {}", line.trim());
		return Ok(None);
	}
	Ok(Some(Remote {
		stdin: in_w.into(),
		stdout: out_r.into(),
		_conn: conn,
	}))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fd_passing() {
		let (a, b) = UnixStream::pair().unwrap();
		let (r, w) = pipe().unwrap();
		send_fds(&a, REQUEST, &[r.as_raw_fd(), w.as_raw_fd()]).unwrap();
		let (payload, fds) = recv_fds(&b, 2).unwrap();
		assert_eq!(payload, REQUEST);
		assert_eq!(fds.len(), 2);
		let mut wf = std::fs::File::from(fds.into_iter().nth(1).unwrap());
		wf.write_all(b"hi").unwrap();
		drop(wf);
		drop(w);
		let mut s = String::new();
		std::fs::File::from(r).read_to_string(&mut s).unwrap();
		assert_eq!(s, "hi");
	}

	#[test]
	fn port_format() {
		assert_eq!(format!(":{PORT:04X}"), ":349D");
	}
}
