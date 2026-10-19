use std::sync::Arc;

use rbxweb::{BinaryType, Client};

use crate::{Error, Result};

/// A binary's deployment (version).
///
/// The default channel is empty in all Roblox APIs ("LIVE" on
/// clientsettings, sometimes "production" in the client itself).
#[derive(Clone)]
pub struct Deployment {
	pub kind: BinaryType,
	pub channel: String,
	pub guid: String,
	/// Client carrying the channel token, used for manifest requests.
	pub client: Option<Arc<Client>>,
}

impl std::fmt::Debug for Deployment {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Deployment")
			.field("channel", &self.channel)
			.field("guid", &self.guid)
			.finish()
	}
}

/// Fetches the latest deployment of a binary type on a channel. An empty
/// channel resolves the authenticated user's channel (or LIVE).
pub fn get_deployment(c: &Client, kind: BinaryType, channel: &str) -> Result<Deployment> {
	let (channel, token) = if channel.is_empty() {
		let uc = c.user_channel(Some(kind))?;
		(uc.channel, uc.token)
	} else {
		(channel.to_owned(), None)
	};

	let client = Arc::new(c.with_channel_token(token));
	match client.client_version(kind, &channel) {
		Ok(cv) => Ok(Deployment {
			kind,
			channel,
			guid: cv.guid,
			client: Some(client),
		}),
		Err(e) if e.code() == Some(5) => Err(Error::BadChannel),
		Err(e) => Err(e.into()),
	}
}

impl Deployment {
	pub(crate) fn get(&self, url: &str) -> Result<Vec<u8>> {
		let local;
		let agent = match &self.client {
			Some(c) => c.agent(),
			None => {
				local = ureq::agent();
				&local
			}
		};
		let mut req = agent.get(url).header("User-Agent", "rbxbin/v0.0.0");
		if let Some(t) = self.client.as_ref().and_then(|c| c.channel_token.as_deref()) {
			req = req.header("Roblox-Channel-Token", t);
		}
		let mut resp = req.call()?;
		let status = resp.status().as_u16();
		if status != 200 {
			return Err(Error::Status {
				url: url.into(),
				status,
			});
		}
		Ok(resp.body_mut().with_config().limit(256 << 20).read_to_vec()?)
	}
}
