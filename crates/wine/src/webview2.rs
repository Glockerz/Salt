//! Edge WebView2 runtime management for a Wineprefix.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::{Error, Prefix, Result};

const API: &str = "https://msedge.api.cdp.microsoft.com/api";

/// Registry key whose `DisplayVersion` holds the installed runtime version.
pub const VERSION_PATH: &str = r"HKLM\Software\Wow6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Microsoft EdgeWebView";

const APP_DEFAULTS: &str = r"HKCU\Software\Wine\AppDefaults\msedgewebview2.exe";

/// Known download channels.
#[derive(Debug, Clone, Copy)]
pub enum Channel {
	Stable,
	StableLegacy,
	Beta,
	Dev,
	Canary,
}

impl Channel {
	fn name(self) -> &'static str {
		match self {
			Channel::Stable => "msedge-stable-win",
			Channel::StableLegacy => "msedge-stable-win7and8",
			Channel::Beta => "msedge-beta-win",
			Channel::Dev => "msedge-dev-win",
			Channel::Canary => "msedge-canary-win",
		}
	}
}

/// A version's available download.
#[derive(Debug, Clone, Deserialize)]
pub struct Download {
	#[serde(rename = "Url")]
	pub url: String,
	#[serde(rename = "FileId")]
	pub file: String,
	#[serde(rename = "SizeInBytes", default)]
	pub size: i64,
	#[serde(rename = "Hashes", default)]
	pub hashes: Hashes,
	#[serde(rename = "DeliveryOptimization", default)]
	pub delivery: Delivery,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Hashes {
	#[serde(rename = "Sha1", default)]
	pub sha1: String,
	#[serde(rename = "Sha256", default)]
	pub sha256: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Delivery {
	#[serde(rename = "CatalogId", default)]
	pub catalog_id: String,
}

impl Download {
	/// The runtime version, and the Edge version for Edge downloads
	/// (empty for the runtime itself).
	pub fn version(&self) -> (&str, &str) {
		let name: Vec<&str> = self.file.trim_end_matches(".exe").split('_').collect();
		match name.len() {
			3 => (name[2], ""),
			4 => (name[2], name[3]),
			_ => ("unknown", "unknown"),
		}
	}
}

/// The Edge update API chains to "Microsoft Root Certificate Authority
/// 2011", which is not in public trust stores, so it is added to the
/// system roots for these requests.
fn agent() -> &'static ureq::Agent {
	static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
	AGENT.get_or_init(|| {
		use ureq::tls::{PemItem, RootCerts, TlsConfig, parse_pem};
		let mut pem = std::fs::read("/etc/ssl/certs/ca-certificates.crt").unwrap_or_default();
		pem.extend_from_slice(MICROSOFT_PEM.as_bytes());
		let certs: Vec<_> = parse_pem(&pem)
			.filter_map(|i| match i {
				Ok(PemItem::Certificate(c)) => Some(c),
				_ => None,
			})
			.collect();
		ureq::Agent::config_builder()
			.tls_config(TlsConfig::builder().root_certs(RootCerts::new_with_certs(&certs)).build())
			.timeout_global(Some(std::time::Duration::from_secs(60)))
			.build()
			.new_agent()
	})
}

impl Channel {
	/// Latest version of the channel. `arch` is one of x86, x64, ARM64.
	pub fn latest(self, arch: &str) -> Result<String> {
		#[derive(Deserialize)]
		struct Resp {
			#[serde(rename = "ContentId")]
			content: Content,
		}
		#[derive(Deserialize)]
		struct Content {
			#[serde(rename = "Version")]
			version: String,
		}
		let url = format!(
			"{API}/v1.1/contents/Browser/namespaces/Default/names/{}-{arch}/versions/latest?action=select",
			self.name()
		);
		let r: Resp = agent()
			.post(&url)
			.header("Accept-Encoding", "identity")
			.header("Content-Type", "application/json")
			.send(r#"{"targetingAttributes":{"Updater":"MicrosoftEdgeUpdate"}}"#)?
			.body_mut()
			.read_json()?;
		Ok(r.content.version)
	}

	/// Downloads (Edge builds and a single runtime) for a version.
	pub fn downloads(self, version: &str, arch: &str) -> Result<Vec<Download>> {
		let url = format!(
			"{API}/v1.1/contents/Browser/namespaces/Default/names/{}-{arch}/versions/{version}/files?action=GenerateDownloadInfo",
			self.name()
		);
		// Microsoft rejects compressed requests.
		Ok(agent()
			.post(&url)
			.header("Accept-Encoding", "identity")
			.header("Content-Type", "application/json")
			.send_empty()?
			.body_mut()
			.read_json()?)
	}

	/// The WebView2 runtime download of a version.
	pub fn runtime(self, version: &str, arch: &str) -> Result<Download> {
		self.downloads(version, arch)?
			.into_iter()
			.find(|d| d.version().1.is_empty())
			.ok_or_else(|| Error::Other("webview2: runtime missing".into()))
	}
}

/// Installs the runtime from a downloaded installer. Outside Proton, a
/// win7 version override is set for msedgewebview2.exe if missing.
pub fn install(pfx: &Prefix, installer: &std::path::Path) -> Result<()> {
	if !pfx.is_proton() {
		// The offline registry avoids spawning Wine just to check; a
		// stale miss only repeats an idempotent `reg add`.
		let present = pfx
			.registry()
			.map(|r| r.query(APP_DEFAULTS).is_some())
			.unwrap_or(false);
		if !present {
			pfx.registry_add(APP_DEFAULTS, "Version", "win7")
				.map_err(|e| Error::Other(format!("version set: {e}")))?;
		}
	}
	pfx.wine(installer, ["--msedgewebview", "--do-not-launch-msedge", "--system-level"])
		.run()
}

/// Runs a version's uninstaller.
pub fn uninstall(pfx: &Prefix, version: &str) -> Result<()> {
	let setup = pfx
		.dir()
		.join("drive_c/Program Files (x86)/Microsoft/EdgeWebView/Application")
		.join(version)
		.join("Installer/setup.exe");
	let r = pfx
		.wine(setup, ["--msedgewebview", "--uninstall", "--system-level", "--force-uninstall"])
		.run();
	// The uninstaller exits with 17 even on success.
	if !installed(pfx, version) {
		return Ok(());
	}
	r
}

/// Reports whether a runtime version is installed.
pub fn installed(pfx: &Prefix, version: &str) -> bool {
	pfx.dir()
		.join("drive_c/Program Files (x86)/Microsoft/EdgeWebView/Application")
		.join(version)
		.exists()
}

const MICROSOFT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIF7TCCA9WgAwIBAgIQP4vItfyfspZDtWnWbELhRDANBgkqhkiG9w0BAQsFADCB
iDELMAkGA1UEBhMCVVMxEzARBgNVBAgTCldhc2hpbmd0b24xEDAOBgNVBAcTB1Jl
ZG1vbmQxHjAcBgNVBAoTFU1pY3Jvc29mdCBDb3Jwb3JhdGlvbjEyMDAGA1UEAxMp
TWljcm9zb2Z0IFJvb3QgQ2VydGlmaWNhdGUgQXV0aG9yaXR5IDIwMTEwHhcNMTEw
MzIyMjIwNTI4WhcNMzYwMzIyMjIxMzA0WjCBiDELMAkGA1UEBhMCVVMxEzARBgNV
BAgTCldhc2hpbmd0b24xEDAOBgNVBAcTB1JlZG1vbmQxHjAcBgNVBAoTFU1pY3Jv
c29mdCBDb3Jwb3JhdGlvbjEyMDAGA1UEAxMpTWljcm9zb2Z0IFJvb3QgQ2VydGlm
aWNhdGUgQXV0aG9yaXR5IDIwMTEwggIiMA0GCSqGSIb3DQEBAQUAA4ICDwAwggIK
AoICAQCygEGqNThNE3IyaCJNuLLx/9VSvGzH9dJKjDbu0cJcfoyKrq8TKG/Ac+M6
ztAlqFo6be+ouFmrEyNozQwph9FvgFyPRH9dkAFSWKxRxV8qh9zc2AodwQO5e7BW
6KPeZGHCnvjzfLnsDbVU/ky2ZU+I8JxImQxCCwl8MVkXeQZ4KI2JOkwDJb5xalwL
54RgpJki49KvhKSn+9GY7Qyp3pSJ4Q6g3MDOmT3qCFK7VnnkH4S6Hri0xElcTzFL
h93dBWcmmYDgcRGjuKVB4qRTufcyKYMME782XgSzS0NHL2vikR7TmE/dQgfI6B0S
/Jmpaz6SfsjWaTr8ZL22CZ3K/QwLopt3YEsDlKQwaRLWQi3BQUzK3Kr9j1uDRprZ
/LHR47PJf0h6zSTwQY9cdNCssBAgBkm3xy0hyFfj0IbzA2j70M5xwYmZSmQBbP3s
MJHPQTySx+W6hh1hhMdfgzlirrSSL0fzC/hV66AfWdC7dJse0Hbm8ukG1xDo+mTe
acY1logC8Ea4PyeZb8txiSk190gWAjWP1Xl8TQLPX+uKg09FcYj5qQ1OcunCnAfP
SRtOBA5jUYxe2ADBVSy2xuDCZU7JNDn1nLPEfuhhbhNfFcRf2X7tHc7uROzLLoax
7Dj2cO2rXBPB2Q8Nx4CyVe0096yb5MPa50c8prWPMd/FS6/r8QIDAQABo1EwTzAL
BgNVHQ8EBAMCAYYwDwYDVR0TAQH/BAUwAwEB/zAdBgNVHQ4EFgQUci06AjGQQ7kU
BU7h6qfHMdEjiTQwEAYJKwYBBAGCNxUBBAMCAQAwDQYJKoZIhvcNAQELBQADggIB
AH9yzw+3xRXbm8BJyiZb/p4T5tPw0tuXX/JLP02zrhmu7deXoKzvqTqjwkGw5biR
nhOBJAPmCf0/V0A5ISRW0RAvS0CpNoZLtFNXmvvxfomPEf4YbFGq6O0JlbXlccmh
6Yd1phV/yX43VF50k8XDZ8wNT2uoFwxtCJJ+i92Bqi1wIcM9BhS7vyRep4TXPw8h
Ir1LAAbblxzYXtTFC1yHblCk6MM4pPvLLMWSZpuFXst6bJN8gClYW1e1QGm6CHmm
ZGIVnYeWRbVmIyADixxzoNOieTPgUFmG2y/lAiXqcyqfABTINseSO+lOAOzYVgm5
M0kS0lQLAausR7aRKX1MtHWAUgHoyoL2n8ysnI8X6i8msKtyrAv+nlEex0NVZ09R
s1fWtuzuUrc66U7h14GIvE+OdbtLqPA1qibUZ2dJsnBMO5PcHd94kIZysjik0dyS
TclY6ysSXNQ7roxrsIPlAT/4CTL2kzU0Iq/dNw13CYArzUgA8YyZGUcFAenRv9FO
0OYoQzeZpApKCNmacXPSqs0xE2N2oTdvkjgefRI8ZjLny23h/FKJ3crWZgWalmG+
oijHHKOnNlA8OqTfSm7mhzvO6/DggTedEzxSjr25HTTGHdUKaj2YKXCMiSrRq4IQ
SB/c9O+lxbtVGjhjhE63bK2VVOxlIhBJF7jAHscPrFRH
-----END CERTIFICATE-----";
