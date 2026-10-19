//! `salt mcp`: a stdio MCP server that bridges to Roblox Studio's
//! StudioMCP.exe running under Salt's Wine.
//!
//! Running StudioMCP.exe directly from an MCP client's wrapper had two
//! problems under Flatpak:
//!
//! - If Studio wasn't running yet, the wrapper's Wine started the
//!   prefix's wineserver inside the wrapper's own sandbox. Studio then
//!   attached to it, and when the client restarted the server (`/mcp`
//!   reconnect), the sandbox and the wineserver with it were torn down,
//!   leaving Studio hung.
//! - The wrapper had to hardcode the Wine build and Studio version paths.
//!
//! This bridge never starts Wine itself. Until Studio's wineserver is
//! up it answers the MCP handshake on its own with an empty tool list;
//! once Studio is running it starts StudioMCP.exe, replays the
//! handshake, proxies everything, and tells the client the tool list
//! changed. When Studio exits it goes back to waiting, so the client
//! never needs to reconnect.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::Config;
use crate::dirs::DIRS;

const PORT: u16 = 13469;
const INIT_ID: &str = "salt-bridge-init";

enum Event {
	Client(String),
	ClientEof,
	Child(u64, String),
	ChildExit(u64),
	Tick,
}

/// The newest installed Studio's StudioMCP.exe.
fn studio_mcp(cfg: &Config) -> Option<PathBuf> {
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

/// Reports whether something listens on the StudioMCP port.
fn port_in_use() -> bool {
	let needle = format!(":{PORT:04X} ");
	["/proc/net/tcp", "/proc/net/tcp6"].iter().any(|f| {
		std::fs::read_to_string(f).is_ok_and(|s| {
			s.lines().skip(1).any(|l| {
				let cols: Vec<&str> = l.split_whitespace().collect();
				cols.len() > 3 && format!("{} ", cols[1]).ends_with(&needle) && cols[3] == "0A"
			})
		})
	})
}

struct Bridge {
	out: std::io::Stdout,
	tx: Sender<Event>,
	cfg: Config,
	pfx: wine::Prefix,
	/// The client's initialize request, replayed to StudioMCP.
	init: Option<Value>,
	initialized: bool,
	child: Option<(u64, Child, ChildStdin)>,
	generation: u64,
	/// Waiting for StudioMCP's answer to the replayed initialize.
	handshaking: bool,
	pending: Vec<String>,
	next_check: Instant,
}

impl Bridge {
	fn send(&mut self, v: &Value) {
		let mut line = v.to_string();
		line.push('\n');
		let _ = self.out.write_all(line.as_bytes());
		let _ = self.out.flush();
	}

	fn send_raw(&mut self, line: &str) {
		let _ = self.out.write_all(line.as_bytes());
		let _ = self.out.write_all(b"\n");
		let _ = self.out.flush();
	}

	fn to_child(&mut self, line: &str) -> bool {
		if let Some((_, _, stdin)) = &mut self.child {
			return stdin.write_all(line.as_bytes()).and_then(|_| stdin.write_all(b"\n")).and_then(|_| stdin.flush()).is_ok();
		}
		false
	}

	fn tools_changed(&mut self) {
		if self.initialized {
			self.send(&json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}));
		}
	}

	/// Answers client messages while Studio isn't available.
	fn answer_offline(&mut self, msg: &Value) {
		let Some(id) = msg.get("id").cloned() else {
			if msg["method"] == "notifications/initialized" {
				self.initialized = true;
			}
			return;
		};
		let result = match msg["method"].as_str().unwrap_or("") {
			"initialize" => {
				self.init = Some(msg.clone());
				let version = msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18");
				json!({
					"protocolVersion": version,
					"capabilities": {"tools": {"listChanged": true}},
					"serverInfo": {"name": "Roblox Studio (Salt bridge)", "version": env!("CARGO_PKG_VERSION")},
					"instructions": "Roblox Studio tools appear once Studio is running under Salt.",
				})
			}
			"ping" => json!({}),
			"tools/list" => json!({"tools": []}),
			"resources/list" => json!({"resources": []}),
			"prompts/list" => json!({"prompts": []}),
			_ => {
				self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {
					"code": -32000,
					"message": "Roblox Studio is not running under Salt yet. Open Studio; its tools will appear automatically.",
				}}));
				return;
			}
		};
		self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
	}

	fn on_client(&mut self, line: String) {
		if self.child.is_some() && !self.handshaking {
			if !self.to_child(&line) {
				log::warn!("StudioMCP stdin closed");
			}
			return;
		}
		let Ok(msg) = serde_json::from_str::<Value>(&line) else {
			return;
		};
		if self.handshaking {
			// Requests arriving mid-handshake are forwarded after it.
			if msg["method"] == "notifications/initialized" {
				self.initialized = true;
			} else {
				self.pending.push(line);
			}
			return;
		}
		self.answer_offline(&msg);
	}

	fn on_child(&mut self, generation: u64, line: String) {
		if self.child.as_ref().map(|c| c.0) != Some(generation) {
			return;
		}
		if self.handshaking {
			let Ok(msg) = serde_json::from_str::<Value>(&line) else {
				return;
			};
			if msg["id"] == INIT_ID {
				self.handshaking = false;
				log::info!("StudioMCP ready: {}", msg["result"]["serverInfo"]);
				self.to_child(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string());
				for p in std::mem::take(&mut self.pending) {
					self.to_child(&p);
				}
				self.tools_changed();
			}
			return;
		}
		self.send_raw(&line);
	}

	fn on_child_exit(&mut self, generation: u64) {
		if self.child.as_ref().map(|c| c.0) != Some(generation) {
			return;
		}
		if let Some((_, mut c, _)) = self.child.take() {
			let _ = c.wait();
		}
		log::warn!("StudioMCP exited; waiting for Studio");
		self.handshaking = false;
		// Requests sent to the dead server get no answer otherwise.
		for p in std::mem::take(&mut self.pending) {
			if let Ok(msg) = serde_json::from_str::<Value>(&p) {
				self.answer_offline(&msg);
			}
		}
		self.tools_changed();
		self.next_check = Instant::now() + Duration::from_secs(3);
	}

	fn try_start(&mut self) {
		if self.child.is_some() || Instant::now() < self.next_check {
			return;
		}
		self.next_check = Instant::now() + Duration::from_secs(1);
		// Never start Wine from here; see the module documentation.
		if !self.pfx.running() {
			return;
		}
		let Some(exe) = studio_mcp(&self.cfg) else {
			log::warn!("StudioMCP.exe not found in {}", DIRS.versions.display());
			self.next_check = Instant::now() + Duration::from_secs(10);
			return;
		};
		let Ok(wine) = self.pfx.wine_path() else {
			log::warn!("Wine not found for the prefix");
			return;
		};

		if port_in_use() {
			log::warn!("Port {PORT} is in use; stopping stale StudioMCP");
			let _ = self.pfx.wine("taskkill", ["/f", "/im", "StudioMCP.exe"]).quiet().run();
			let deadline = Instant::now() + Duration::from_secs(5);
			while port_in_use() && Instant::now() < deadline {
				std::thread::sleep(Duration::from_millis(100));
			}
		}

		log::info!("Starting {}", exe.display());
		let mut cmd = Command::new(wine);
		cmd.arg(&exe).arg("--stdio");
		for (k, v) in &self.pfx.env {
			cmd.env(k, v);
		}
		cmd.env("WINEPREFIX", self.pfx.dir())
			.env("WINEDEBUG", "-all")
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit());
		let mut child = match cmd.spawn() {
			Ok(c) => c,
			Err(e) => {
				log::error!("Failed to start StudioMCP: {e}");
				return;
			}
		};
		self.generation += 1;
		let generation = self.generation;
		let stdin = child.stdin.take().expect("piped");
		let stdout = child.stdout.take().expect("piped");
		let tx = self.tx.clone();
		std::thread::spawn(move || {
			for line in BufReader::new(stdout).lines() {
				match line {
					Ok(l) if !l.trim().is_empty() => {
						if tx.send(Event::Child(generation, l)).is_err() {
							return;
						}
					}
					Ok(_) => {}
					Err(_) => break,
				}
			}
			let _ = tx.send(Event::ChildExit(generation));
		});
		self.child = Some((generation, child, stdin));

		match self.init.clone() {
			Some(mut init) => {
				init["id"] = json!(INIT_ID);
				self.handshaking = true;
				self.to_child(&init.to_string());
			}
			// The client hasn't initialized yet; it will talk to
			// StudioMCP directly.
			None => {}
		}
	}

	fn shutdown(&mut self) {
		if let Some((_, mut c, stdin)) = self.child.take() {
			drop(stdin);
			// StudioMCP exits on EOF; give it a moment, then make sure.
			let deadline = Instant::now() + Duration::from_secs(2);
			while Instant::now() < deadline {
				if let Ok(Some(_)) = c.try_wait() {
					return;
				}
				std::thread::sleep(Duration::from_millis(50));
			}
			let _ = c.kill();
			let _ = c.wait();
		}
	}
}

pub fn run() -> std::process::ExitCode {
	crate::logging::init_stderr_only();
	let cfg = Config::load().unwrap_or_else(|e| {
		log::error!("config error: {e}; using defaults");
		Config::default()
	});
	crate::logging::set_debug(cfg.debug);
	let pfx = cfg.prefix();

	let (tx, rx) = channel();
	{
		let tx = tx.clone();
		std::thread::spawn(move || {
			let stdin = std::io::stdin();
			for line in stdin.lock().lines() {
				match line {
					Ok(l) if !l.trim().is_empty() => {
						if tx.send(Event::Client(l)).is_err() {
							return;
						}
					}
					Ok(_) => {}
					Err(_) => break,
				}
			}
			let _ = tx.send(Event::ClientEof);
		});
	}
	{
		let tx = tx.clone();
		std::thread::spawn(move || {
			while tx.send(Event::Tick).is_ok() {
				std::thread::sleep(Duration::from_millis(500));
			}
		});
	}

	let mut b = Bridge {
		out: std::io::stdout(),
		tx,
		cfg,
		pfx,
		init: None,
		initialized: false,
		child: None,
		generation: 0,
		handshaking: false,
		pending: Vec::new(),
		next_check: Instant::now(),
	};
	log::info!("Salt MCP bridge started (prefix {})", b.pfx.dir().display());
	b.try_start();

	for ev in rx {
		match ev {
			Event::Client(l) => b.on_client(l),
			Event::ClientEof => break,
			Event::Child(g, l) => b.on_child(g, l),
			Event::ChildExit(g) => b.on_child_exit(g),
			Event::Tick => b.try_start(),
		}
	}
	b.shutdown();
	std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
	#[test]
	fn port_format() {
		// 13469 is 0x349D in /proc/net/tcp's local_address column.
		assert_eq!(format!(":{:04X} ", super::PORT), ":349D ");
	}
}
