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
//! This bridge never runs Wine. Until Studio is running under Salt it
//! answers the MCP handshake on its own with an empty tool list; once
//! Studio is up it asks the Salt instance hosting Wine to start
//! StudioMCP.exe there (see winehost.rs for why it must run there),
//! replays the handshake, proxies everything, and tells the client the
//! tool list changed. When Studio exits it goes back to waiting, so the
//! client never needs to reconnect.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::winehost::{self, Remote};

const INIT_ID: &str = "salt-bridge-init";

enum Event {
	Client(String),
	ClientEof,
	Child(u64, String),
	ChildExit(u64),
	Tick,
}

struct Bridge {
	out: std::io::Stdout,
	tx: Sender<Event>,
	/// The client's initialize request, replayed to StudioMCP.
	init: Option<Value>,
	initialized: bool,
	child: Option<(u64, Remote)>,
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
		if let Some((_, Remote { stdin, .. })) = &mut self.child {
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
		self.child = None;
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
		let remote = match winehost::request() {
			Ok(Some(r)) => r,
			Ok(None) => return,
			Err(e) => {
				log::warn!("Starting StudioMCP failed: {e}");
				self.next_check = Instant::now() + Duration::from_secs(5);
				return;
			}
		};
		log::info!("Studio is running; connected to StudioMCP");
		self.generation += 1;
		let generation = self.generation;
		let stdout = match remote.stdout.try_clone() {
			Ok(f) => f,
			Err(e) => return log::error!("StudioMCP stdout: {e}"),
		};
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
		self.child = Some((generation, remote));

		// Before the client initialized, it talks to StudioMCP directly.
		if let Some(mut init) = self.init.clone() {
			init["id"] = json!(INIT_ID);
			self.handshaking = true;
			self.to_child(&init.to_string());
		}
	}

	fn shutdown(&mut self) {
		// Closing stdin ends StudioMCP; closing the socket makes Salt
		// kill it if it doesn't.
		self.child = None;
	}
}

pub fn run() -> std::process::ExitCode {
	crate::logging::init_stderr_only();

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
		init: None,
		initialized: false,
		child: None,
		generation: 0,
		handshaking: false,
		pending: Vec::new(),
		next_check: Instant::now(),
	};
	log::info!("Salt MCP bridge started; waiting for Studio via {}", winehost::socket_path().display());
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
