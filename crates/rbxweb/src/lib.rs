//! Roblox web API client, ported from `github.com/sewnie/rbxweb` and
//! limited to the endpoints Vinegar uses.

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("{0}")]
	Api(Errors),
	#[error("{0}")]
	Message(String),
	#[error("bad response: {0}")]
	Status(u16),
	#[error("http: {0}")]
	Http(#[from] Box<ureq::Error>),
	#[error("decode: {0}")]
	Decode(#[from] serde_json::Error),
}

impl From<ureq::Error> for Error {
	fn from(e: ureq::Error) -> Self {
		Error::Http(Box::new(e))
	}
}

impl Error {
	/// The first API error code, if any.
	pub fn code(&self) -> Option<i64> {
		match self {
			Error::Api(e) => e.errors.first().map(|e| e.code),
			_ => None,
		}
	}
}

pub type Result<T> = std::result::Result<T, Error>;

/// One entry of an API error response.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
	pub code: i64,
	#[serde(default)]
	pub message: String,
}

/// The errors response model of the API.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Errors {
	#[serde(default)]
	pub errors: Vec<ApiError>,
}

impl fmt::Display for Errors {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		for (i, e) in self.errors.iter().enumerate() {
			if i > 0 {
				f.write_str("; ")?;
			}
			write!(f, "response code {}: {}", e.code, e.message)?;
		}
		Ok(())
	}
}

/// A supported Roblox platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryType {
	WindowsPlayer,
	WindowsStudio64,
	MacPlayer,
	MacStudio,
}

impl BinaryType {
	pub fn as_str(self) -> &'static str {
		match self {
			BinaryType::WindowsPlayer => "WindowsPlayer",
			BinaryType::WindowsStudio64 => "WindowsStudio64",
			BinaryType::MacPlayer => "MacPlayer",
			BinaryType::MacStudio => "MacStudio",
		}
	}

	/// "Player" or "Studio".
	pub fn short(self) -> &'static str {
		match self {
			BinaryType::WindowsPlayer | BinaryType::MacPlayer => "Player",
			BinaryType::WindowsStudio64 | BinaryType::MacStudio => "Studio",
		}
	}
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientVersion {
	pub version: String,
	#[serde(rename = "clientVersionUpload")]
	pub guid: String,
	#[serde(rename = "bootstrapperVersion", default)]
	pub bootstrapper: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserChannel {
	#[serde(rename = "channelName")]
	pub channel: String,
	#[serde(rename = "channelAssignmentType", default)]
	pub assignment: i64,
	#[serde(default)]
	pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaceDetail {
	#[serde(rename = "placeId")]
	pub id: i64,
	pub name: String,
	#[serde(rename = "universeId")]
	pub universe_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Thumbnail {
	#[serde(rename = "targetId")]
	pub target_id: i64,
	pub state: String,
	#[serde(rename = "imageUrl", default)]
	pub image_url: Option<String>,
}

/// A Roblox API client. Thread safe; the session cookie and CSRF token
/// are updated in place.
pub struct Client {
	agent: ureq::Agent,
	pub base_domain: String,
	security: Mutex<String>,
	csrf: Mutex<String>,
	/// Sent as `Roblox-Channel-Token`, for private deployment channels.
	pub channel_token: Option<String>,
	debug: AtomicBool,
}

impl Default for Client {
	fn default() -> Self {
		Self::new()
	}
}

impl Client {
	pub fn new() -> Self {
		let agent = ureq::Agent::config_builder()
			.http_status_as_error(false)
			.timeout_global(Some(Duration::from_secs(30)))
			.user_agent("rbxweb/v0.0.0")
			.build()
			.new_agent();
		Client {
			agent,
			base_domain: "roblox.com".into(),
			security: Mutex::new(String::new()),
			csrf: Mutex::new(String::new()),
			channel_token: None,
			debug: AtomicBool::new(false),
		}
	}

	/// A client sharing this client's session, for a private channel.
	pub fn with_channel_token(&self, token: Option<String>) -> Self {
		let c = Client::new();
		*c.security.lock().unwrap() = self.security();
		c.debug.store(self.debug.load(Ordering::Relaxed), Ordering::Relaxed);
		Client {
			channel_token: token.filter(|t| !t.is_empty()),
			..c
		}
	}

	/// Sets the `.ROBLOSECURITY` session cookie.
	pub fn set_security(&self, s: impl Into<String>) {
		*self.security.lock().unwrap() = s.into();
	}

	pub fn security(&self) -> String {
		self.security.lock().unwrap().clone()
	}

	pub fn set_debug(&self, on: bool) {
		self.debug.store(on, Ordering::Relaxed);
	}

	pub fn agent(&self) -> &ureq::Agent {
		&self.agent
	}

	fn request(&self, method: &str, url: &str) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error> {
		let security = self.security();
		let csrf = self.csrf.lock().unwrap().clone();
		let mut req = ureq::http::Request::builder()
			.method(method)
			.uri(url)
			.header("Accept", "application/json")
			.header("Accept-Encoding", "identity");
		if !csrf.is_empty() {
			req = req.header("X-CSRF-TOKEN", csrf);
		}
		if !security.is_empty() {
			req = req.header("Cookie", format!(".ROBLOSECURITY={security}"));
		}
		if let Some(t) = &self.channel_token {
			req = req.header("Roblox-Channel-Token", t);
		}
		let req = req.body(()).map_err(ureq::Error::from)?;
		self.agent.run(req)
	}

	/// Performs a request against `https://{service}.{base_domain}/{path}`,
	/// retrying once with a fresh CSRF token on a 403 that provides one.
	pub fn execute<T: DeserializeOwned>(&self, method: &str, service: &str, path: &str) -> Result<T> {
		let url = if service.is_empty() {
			format!("https://{}/{path}", self.base_domain)
		} else {
			format!("https://{service}.{}/{path}", self.base_domain)
		};
		let debug = self.debug.load(Ordering::Relaxed);
		if debug {
			log::debug!("rbxweb request method={method} url={url}");
		}

		let mut resp = self.request(method, &url)?;
		if resp.status() == 403
			&& let Some(t) = resp.headers().get("x-csrf-token").and_then(|v| v.to_str().ok())
			&& !t.is_empty()
		{
			*self.csrf.lock().unwrap() = t.to_owned();
			resp = self.request(method, &url)?;
		}

		for c in resp.headers().get_all("set-cookie") {
			if let Some(v) = c.to_str().ok().and_then(|c| c.strip_prefix(".ROBLOSECURITY="))
				&& let Some(v) = v.split(';').next()
			{
				self.set_security(v);
			}
		}

		let status = resp.status().as_u16();
		if debug {
			log::debug!("rbxweb response status={status}");
		}
		let body = resp.body_mut().read_to_vec()?;
		if (200..300).contains(&status) {
			return Ok(serde_json::from_slice(&body)?);
		}
		if let Ok(errs) = serde_json::from_slice::<Errors>(&body) {
			return Err(Error::Api(errs));
		}
		// Some undocumented APIs return a single string as an error.
		if let Ok(s) = serde_json::from_slice::<String>(&body) {
			return Err(Error::Message(s));
		}
		Err(Error::Status(status))
	}

	/// Client version information for a binary type and channel.
	pub fn client_version(&self, bt: BinaryType, channel: &str) -> Result<ClientVersion> {
		let mut path = format!("v2/client-version/{}", bt.as_str());
		if !channel.is_empty() {
			path.push_str("/channel/");
			path.push_str(channel);
		}
		self.execute("GET", "clientsettings", &path)
	}

	/// The deployment channel of the authenticated user.
	pub fn user_channel(&self, bt: Option<BinaryType>) -> Result<UserChannel> {
		let path = match bt {
			Some(bt) => format!("v2/user-channel?binaryType={}", bt.as_str()),
			None => "v2/user-channel".into(),
		};
		self.execute("GET", "clientsettings", &path)
	}

	pub fn place_detail(&self, place: i64) -> Result<Option<PlaceDetail>> {
		let v: Vec<PlaceDetail> =
			self.execute("GET", "games", &format!("v1/games/multiget-place-details?placeIds={place}"))?;
		Ok(v.into_iter().next())
	}

	/// A universe's icon, e.g. size "512x512", format "Png".
	pub fn game_icon(&self, universe: i64, size: &str, format: &str, circular: bool) -> Result<Option<Thumbnail>> {
		#[derive(Deserialize)]
		struct Resp {
			data: Vec<Thumbnail>,
		}
		let r: Resp = self.execute(
			"GET",
			"thumbnails",
			&format!(
				"v1/games/icons?universeIds={universe}&returnPolicy=PlaceHolder&size={size}&format={format}&isCircular={circular}"
			),
		)?;
		Ok(r.data.into_iter().next())
	}
}
