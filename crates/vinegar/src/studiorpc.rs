//! Roblox Studio Discord Rich Presence.
//!
//! Runs on its own thread, fed through a bounded channel. In the Go
//! version, the place lookups (two HTTPS requests) ran inside the Wine
//! output reader, so opening a place or starting a playtest stalled
//! every Studio thread that was writing debug output at the time.

use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::discord::{Activity, Assets, Client, Timestamps};

const APP_ID: &str = "1159891020956323923";
const OPEN: &str = "[FLog::CloseDataModel] Setting place ID ";
const EDIT: &str = "[FLog::RobloxDocManager] Setting current doc to type ";

/// Reports whether the line is one Rich Presence reacts to; checked
/// before anything is allocated or sent.
pub fn relevant(line: &str) -> bool {
	line.starts_with(OPEN) || line.starts_with(EDIT)
}

#[derive(Clone)]
pub struct StudioRpc {
	tx: SyncSender<String>,
}

fn now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl StudioRpc {
	pub fn spawn(rbx: Arc<rbxweb::Client>) -> Self {
		let (tx, rx) = sync_channel::<String>(256);
		std::thread::Builder::new()
			.name("studiorpc".into())
			.spawn(move || {
				let mut s = State {
					client: Client::new(APP_ID),
					rbx,
					presence: Activity {
						assets: Assets {
							large_image: "studio".into(),
							large_text: "Vinegar".into(),
							..Default::default()
						},
						..Default::default()
					},
					place: None,
					thumbnail: None,
				};
				for line in rx {
					if let Err(e) = s.handle(&line) {
						log::error!("Discord Rich Presence handling failed: {e}");
					}
				}
			})
			.expect("spawn studiorpc");
		StudioRpc { tx }
	}

	/// Queues a line without ever blocking the caller.
	pub fn send(&self, line: &str) {
		if let Err(TrySendError::Full(_)) = self.tx.try_send(line.to_owned()) {
			log::debug!("studiorpc: queue full, dropping line");
		}
	}
}

struct State {
	client: Client,
	rbx: Arc<rbxweb::Client>,
	presence: Activity,
	place: Option<rbxweb::PlaceDetail>,
	thumbnail: Option<String>,
}

impl State {
	fn handle(&mut self, line: &str) -> Result<(), Box<dyn std::error::Error>> {
		if let Some(id) = line.strip_prefix(OPEN) {
			let id: i64 = id.trim().parse()?;
			log::info!("studiorpc: Opened Place placeid={id}");
			self.presence.timestamps = Some(Timestamps { start: now() });
			if id == 0 {
				// Unpublished place, or the edit DataModel being reset.
				self.place = None;
				self.thumbnail = None;
				return Ok(());
			}
			self.place = self.rbx.place_detail(id).unwrap_or_else(|e| {
				log::error!("studiorpc: Failed to fetch place detail: {e}");
				None
			});
			self.thumbnail = self.place.as_ref().and_then(|p| {
				self.rbx
					.game_icon(p.universe_id, "512x512", "Png", false)
					.map_err(|e| log::error!("studiorpc: Failed to fetch game icon: {e}"))
					.ok()
					.flatten()
					.and_then(|t| t.image_url)
			});
			return Ok(());
		}

		let Some(kind) = line.strip_prefix(EDIT) else {
			return Ok(());
		};
		log::info!("studiorpc: Changed main view");
		let p = &mut self.presence;
		match kind.trim() {
			"-1" => {
				self.place = None;
				p.details = "Idling".into();
				p.state.clear();
				p.assets.large_image.clear();
				p.assets.small_image.clear();
				p.timestamps = Some(Timestamps { start: now() });
			}
			"0" => {
				p.details = match &self.place {
					Some(pl) => format!("Developing {}", pl.name),
					None => "Developing a place".into(),
				};
				p.state.clear();
				p.assets.small_image = "place".into();
				p.assets.large_image = self.thumbnail.clone().unwrap_or_default();
			}
			"2" => {
				p.state = "Editing Script".into();
				p.assets.small_image = "script".into();
			}
			_ => {}
		}
		self.client.set_activity(&self.presence)?;
		Ok(())
	}
}
