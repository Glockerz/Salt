//! Salt's configuration, compatible with the Go version's config.toml.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rbxbin::FFlags;
use serde::{Deserialize, Serialize};

use crate::dirs::DIRS;
use crate::sysinfo::INFO;

pub const DXVK_VERSION: &str = "3.1";
pub const DXVK_SAREK_VERSION: &str = "Sarek-1.11.0-async";
pub const WEBVIEW_VERSION: &str = "144.0.3719.92";
pub const DESKTOP_RESOLUTION: &str = "1814x1024";

/// Order must match the renderer model in the manager UI.
pub const RENDERERS: &[&str] = &["D3D11", "D3D11FL10", "DXVK", "DXVK-Sarek", "Vulkan"];

/// Order must match the appearance model in the manager UI.
pub const THEMES: &[&str] = &["auto", "light", "dark"];

/// Where Roblox's own log is read from.
pub const LOG_SOURCES: &[&str] = &["file", "debugger"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Studio {
	pub webview: String,
	pub wineroot: String,
	pub renderer: String,
	#[serde(rename = "virtual_desktop")]
	pub desktop: String,
	#[serde(rename = "gpu")]
	pub forced_gpu: String,
	pub launcher: String,
	pub discord_rpc: bool,
	pub gamemode: bool,
	pub env: BTreeMap<String, String>,
	pub fflags: FFlags,
	pub forced_version: String,
	pub channel: String,

	/// "file" tails the log files Studio writes itself. "debugger"
	/// reads them from Wine's debug channel (`warn+seh`) like the Go
	/// version, which makes every Studio thread that logs write through
	/// Wine's stderr pipe synchronously.
	pub log_source: String,

	/// Deprecated; any value selects the DXVK renderer.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub dxvk: Option<toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
	pub studio: Studio,
	/// Merged into studio.env; kept for backwards compatibility.
	pub env: BTreeMap<String, String>,
	pub debug: bool,
	/// Salt's icon: "auto" follows the desktop theme, or "black"/"white".
	pub icon: String,
	/// Salt's appearance: "auto" follows the system, or "light"/"dark".
	pub theme: String,
	/// Install new Salt releases in the background (flatpak only).
	pub auto_update: bool,
}

impl Default for Studio {
	fn default() -> Self {
		let cards = &INFO.cards;
		Studio {
			webview: WEBVIEW_VERSION.into(),
			// Kombucha builds are glibc-only.
			wineroot: if INFO.libc.contains("musl") {
				String::new()
			} else {
				DIRS.wine_path.display().to_string()
			},
			renderer: "DXVK".into(),
			desktop: String::new(),
			// Prefer the discrete GPU on hybrid systems.
			forced_gpu: if cards.len() >= 2 && cards[0].embedded {
				cards[1].addr()
			} else {
				String::new()
			},
			launcher: String::new(),
			discord_rpc: true,
			gamemode: true,
			env: BTreeMap::new(),
			fflags: FFlags::new(),
			forced_version: String::new(),
			channel: String::new(),
			log_source: "file".into(),
			dxvk: None,
		}
	}
}

impl Default for Config {
	fn default() -> Self {
		Config {
			studio: Studio::default(),
			env: BTreeMap::new(),
			debug: false,
			icon: "auto".into(),
			theme: "auto".into(),
			auto_update: true,
		}
	}
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("renderer must be one of {RENDERERS:?}")]
	Renderer,
	#[error("log_source must be one of {LOG_SOURCES:?}")]
	LogSource,
	#[error("icon must be one of {:?}", crate::icon::SETTINGS)]
	Icon,
	#[error("theme must be one of {THEMES:?}")]
	Theme,
	#[error("{0}")]
	Toml(#[from] toml::de::Error),
	#[error("{0}")]
	Encode(#[from] toml::ser::Error),
	#[error(transparent)]
	Io(#[from] std::io::Error),
}

impl Config {
	/// Loads the configuration file, or the defaults if there is none.
	pub fn load() -> Result<Config, Error> {
		match std::fs::read_to_string(&DIRS.config_path) {
			Ok(s) => Config::parse(&s),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
			Err(e) => Err(e.into()),
		}
	}

	pub fn parse(s: &str) -> Result<Config, Error> {
		let mut cfg: Config = toml::from_str(s)?;
		let legacy = std::mem::take(&mut cfg.env);
		cfg.studio.env.extend(legacy);
		if cfg.studio.dxvk.take().is_some() {
			log::warn!("The DXVK option alongside its versioning has been deprecated, setting Renderer");
			cfg.studio.renderer = "DXVK".into();
		}
		cfg.validate()?;
		Ok(cfg)
	}

	pub fn validate(&self) -> Result<(), Error> {
		if !RENDERERS.contains(&self.studio.renderer.as_str()) {
			return Err(Error::Renderer);
		}
		if !LOG_SOURCES.contains(&self.studio.log_source.as_str()) {
			return Err(Error::LogSource);
		}
		if !crate::icon::SETTINGS.contains(&self.icon.as_str()) {
			return Err(Error::Icon);
		}
		if !THEMES.contains(&self.theme.as_str()) {
			return Err(Error::Theme);
		}
		Ok(())
	}

	/// The configuration with defaults stripped, as TOML.
	pub fn encode(&self) -> Result<String, Error> {
		let def = toml::Value::try_from(Config::default())?;
		let cur = toml::Value::try_from(self)?;
		let diff = match (def, cur) {
			(toml::Value::Table(d), toml::Value::Table(c)) => diff(&d, &c),
			_ => toml::Table::new(),
		};
		Ok(toml::to_string(&diff)?)
	}

	pub fn save(&self) -> Result<(), Error> {
		std::fs::create_dir_all(&DIRS.config)?;
		// Write atomically so a crash mid-save can't truncate the file.
		let tmp = DIRS.config_path.with_extension("toml.tmp");
		std::fs::write(&tmp, self.encode()?)?;
		std::fs::rename(tmp, &DIRS.config_path)?;
		Ok(())
	}
}

fn diff(def: &toml::Table, cur: &toml::Table) -> toml::Table {
	let mut out = toml::Table::new();
	for (k, v) in cur {
		match (def.get(k), v) {
			(Some(toml::Value::Table(d)), toml::Value::Table(c)) => {
				let sub = diff(d, c);
				if !sub.is_empty() {
					out.insert(k.clone(), toml::Value::Table(sub));
				}
			}
			(Some(d), c) if d == c => {}
			_ => {
				out.insert(k.clone(), v.clone());
			}
		}
	}
	out
}

impl Studio {
	pub fn dxvk_version(&self) -> Option<&'static str> {
		match self.renderer.as_str() {
			"DXVK" => Some(DXVK_VERSION),
			"DXVK-Sarek" => Some(DXVK_SAREK_VERSION),
			_ => None,
		}
	}

	pub fn launcher_path(&self) -> Option<PathBuf> {
		let name = self.launcher.split_whitespace().next()?;
		if name.contains('/') {
			return Some(PathBuf::from(name));
		}
		let path = std::env::var_os("PATH")?;
		std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
	}
}

/// Location of the Vulkan layer manifest shipped next to the executable.
fn layer_dir() -> Option<PathBuf> {
	let exe = std::env::current_exe().ok()?;
	let dir = exe.parent()?.join("../share/vulkan/explicit_layer.d");
	dir.join("VkLayer_SALT_SaltLayer.json").exists().then_some(dir)
}

impl Config {
	/// Builds the Wineprefix for Studio with the environment the
	/// configuration calls for.
	pub fn prefix(&self) -> wine::Prefix {
		let mut pfx = wine::Prefix::new(DIRS.prefixes.join("studio"), &self.studio.wineroot);
		let mut env = self.studio.env.clone();

		if let Some(card) = INFO.cards.iter().find(|c| c.addr() == self.studio.forced_gpu) {
			log::debug!("Using GPU index={} card={}", card.index, card.product);
			env.insert("DRI_PRIME".into(), format!("{}:{}!", card.vendor_id, card.product_id));
			// Unset problematic variables that switcheroo-control may set.
			env.insert("__VK_LAYER_NV_optimus".into(), String::new());
			env.insert("VK_LOADER_DRIVERS_SELECT".into(), String::new());
			let vendors = if INFO.flatpak {
				"/usr/lib/x86_64-linux-gnu/GL/glvnd/egl_vendor.d/"
			} else {
				"/usr/share/glvnd/egl_vendor.d/"
			};
			let nvidia = card.driver.starts_with("nvidia");
			env.insert(
				"__EGL_VENDOR_LIBRARY_FILENAMES".into(),
				format!("{vendors}{}", if nvidia { "10_nvidia.json" } else { "50_mesa.json" }),
			);
			env.insert("__GLX_VENDOR_LIBRARY_NAME".into(), if nvidia { "nvidia" } else { "mesa" }.into());
			env.insert("__NV_PRIME_RENDER_OFFLOAD".into(), if nvidia { "1" } else { "0" }.into());
		}

		// Wine applies channel-specific options on top of the defaults in
		// effect when the channel is first named, so global clears must
		// come before "warn+seh"; the Go version put them after, which
		// left fixme:seh enabled and flooded the log.
		let mut debug: Vec<String> = env
			.remove("WINEDEBUG")
			.map(|d| d.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect())
			.unwrap_or_default();
		if !self.debug {
			debug.extend(["fixme-all", "err-kerberos", "err-ntlm", "err-combase"].map(String::from));
		}
		if self.studio.log_source == "debugger" {
			debug.push("warn+seh".into());
		}
		if !debug.is_empty() {
			env.insert("WINEDEBUG".into(), debug.join(","));
		}

		env.insert("XR_LOADER_DEBUG".into(), "none".into());
		let overrides = env.entry("WINEDLLOVERRIDES".into()).or_default();
		if !overrides.is_empty() {
			overrides.push(';');
		}
		overrides.push_str("dxdiagn,winemenubuilder.exe,mscoree,mshtml=");

		let (angle, d3d) = match self.studio.renderer.as_str() {
			"D3D11" | "D3D11FL10" => ("gl", "gl"),
			_ => ("vulkan", "vulkan"),
		};
		env.insert(
			"WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS".into(),
			format!("--in-process-gpu --use-angle={angle}"),
		);
		env.insert("WINE_D3D_CONFIG".into(), format!("renderer={d3d}"));
		if self.studio.renderer == "Vulkan" {
			env.insert("VK_LOADER_LAYERS_ENABLE".into(), "VK_LAYER_SALT_SaltLayer".into());
			if let Some(dir) = layer_dir() {
				env.insert("VK_ADD_LAYER_PATH".into(), dir.display().to_string());
			}
		}

		let dxvk = self.studio.dxvk_version().is_some();
		if dxvk {
			if !self.debug {
				env.insert("DXVK_LOG_LEVEL".into(), "warn".into());
			}
			env.insert("DXVK_LOG_PATH".into(), "none".into());
			env.insert("DXVK_STATE_CACHE_PATH".into(), DIRS.cache.display().to_string());
		}

		pfx.env = env.into_iter().collect();
		if dxvk {
			wine::dxvk::env_override(&mut pfx);
		}
		log::debug!("Using Prefix environment {:?}", pfx.env);
		pfx
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn encode_defaults_empty() {
		assert_eq!(Config::default().encode().unwrap(), "");
	}

	#[test]
	fn encode_diff_roundtrip() {
		let mut cfg = Config::default();
		cfg.debug = true;
		cfg.studio.gamemode = false;
		cfg.studio.webview = String::new();
		cfg.studio.fflags.insert("DFIntFoo".into(), rbxbin::FFlag::Int(960));
		cfg.studio.env.insert("FOO".into(), "1".into());
		let s = cfg.encode().unwrap();
		assert!(s.contains("debug = true"));
		assert!(s.contains("gamemode = false"));
		assert!(s.contains("webview = \"\""));
		assert!(s.contains("DFIntFoo = 960"));
		assert!(!s.contains("renderer"));
		assert_eq!(Config::parse(&s).unwrap(), cfg);
	}

	#[test]
	fn legacy() {
		let cfg = Config::parse("[env]\nA = \"1\"\n[studio]\ndxvk = \"2.3\"\nrenderer = \"D3D11\"\n").unwrap();
		assert_eq!(cfg.studio.env["A"], "1");
		assert!(cfg.env.is_empty());
		assert_eq!(cfg.studio.renderer, "DXVK");
		assert!(Config::parse("[studio]\nrenderer = \"Metal\"\n").is_err());
	}

	#[test]
	fn winedebug_order() {
		let mut cfg = Config::default();
		cfg.studio.log_source = "debugger".into();
		cfg.studio.env.insert("WINEDEBUG".into(), "+relay".into());
		let pfx = cfg.prefix();
		let d = &pfx.env.iter().find(|(k, _)| k == "WINEDEBUG").unwrap().1;
		assert_eq!(d, "+relay,fixme-all,err-kerberos,err-ntlm,err-combase,warn+seh");
		let o = &pfx.env.iter().find(|(k, _)| k == "WINEDLLOVERRIDES").unwrap().1;
		assert_eq!(o, &format!("dxdiagn,winemenubuilder.exe,mscoree,mshtml=;{}", wine::dxvk::OVERRIDES));
	}
}
