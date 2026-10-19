//! Minimal Discord Rich Presence over Discord's local IPC socket.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Activity {
	#[serde(skip_serializing_if = "String::is_empty")]
	pub details: String,
	#[serde(skip_serializing_if = "String::is_empty")]
	pub state: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub timestamps: Option<Timestamps>,
	pub assets: Assets,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Timestamps {
	pub start: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Assets {
	#[serde(skip_serializing_if = "String::is_empty")]
	pub large_image: String,
	#[serde(skip_serializing_if = "String::is_empty")]
	pub large_text: String,
	#[serde(skip_serializing_if = "String::is_empty")]
	pub small_image: String,
}

pub struct Client {
	id: String,
	conn: Option<UnixStream>,
	nonce: u64,
}

/// Candidate socket paths, covering native, Flatpak and Snap Discord.
fn sockets() -> Vec<PathBuf> {
	let run = std::env::var_os("XDG_RUNTIME_DIR")
		.map(PathBuf::from)
		.unwrap_or_else(|| format!("/run/user/{}", unsafe { libc::getuid() }).into());
	let dirs = [
		run.clone(),
		run.join("app/com.discordapp.Discord"),
		run.join("app/com.discordapp.DiscordCanary"),
		run.join("app/dev.vencord.Vesktop"),
		run.join(".flatpak/dev.vencord.Vesktop/xdg-run"),
		run.join("snap.discord"),
	];
	dirs.iter()
		.flat_map(|d| (0..10).map(move |i| d.join(format!("discord-ipc-{i}"))))
		.collect()
}

impl Client {
	pub fn new(id: &str) -> Self {
		Client {
			id: id.into(),
			conn: None,
			nonce: 0,
		}
	}

	fn send(conn: &mut UnixStream, op: u32, payload: &serde_json::Value) -> std::io::Result<()> {
		let data = serde_json::to_vec(payload)?;
		let mut frame = Vec::with_capacity(8 + data.len());
		frame.extend_from_slice(&op.to_le_bytes());
		frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
		frame.extend_from_slice(&data);
		conn.write_all(&frame)
	}

	fn recv(conn: &mut UnixStream) -> std::io::Result<serde_json::Value> {
		let mut hdr = [0u8; 8];
		conn.read_exact(&mut hdr)?;
		let len = u32::from_le_bytes(hdr[4..].try_into().unwrap()) as usize;
		let mut buf = vec![0u8; len.min(1 << 20)];
		conn.read_exact(&mut buf)?;
		Ok(serde_json::from_slice(&buf).unwrap_or_default())
	}

	fn connect(&mut self) -> std::io::Result<&mut UnixStream> {
		if self.conn.is_none() {
			let mut last = std::io::Error::from(std::io::ErrorKind::NotFound);
			for p in sockets() {
				match UnixStream::connect(&p) {
					Ok(mut c) => {
						c.set_read_timeout(Some(Duration::from_secs(5)))?;
						c.set_write_timeout(Some(Duration::from_secs(5)))?;
						Self::send(&mut c, 0, &serde_json::json!({"v": 1, "client_id": self.id}))?;
						Self::recv(&mut c)?; // READY
						self.conn = Some(c);
						break;
					}
					Err(e) => last = e,
				}
			}
			if self.conn.is_none() {
				return Err(last);
			}
		}
		Ok(self.conn.as_mut().unwrap())
	}

	pub fn set_activity(&mut self, a: &Activity) -> std::io::Result<()> {
		self.nonce += 1;
		let payload = serde_json::json!({
			"cmd": "SET_ACTIVITY",
			"args": {"pid": std::process::id(), "activity": a},
			"nonce": self.nonce.to_string(),
		});
		let r = self.connect().and_then(|c| {
			Self::send(c, 1, &payload)?;
			Self::recv(c).map(drop)
		});
		if r.is_err() {
			// Reconnect next time, e.g. after Discord restarts.
			self.conn = None;
		}
		r
	}
}
