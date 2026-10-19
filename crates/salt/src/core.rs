//! Application state and bootstrapper logic, independent of the UI.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use rbxbin::{BinaryType, Deployment};
use wine::{Prefix, Registry};

use crate::config::Config;
use crate::dirs::{self, DIRS, TAG_PREFIX};
use crate::i18n::l;
use crate::logging::{self, Level};
use crate::netutil;
use crate::robloxlog::{self, LogEvents};
use crate::studiorpc::StudioRpc;
use crate::sysinfo::INFO;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T = ()> = std::result::Result<T, Error>;

pub const STUDIO: BinaryType = BinaryType::WindowsStudio64;
const STUDIO_REG_PATH: &str = r"HKCU\Software\Roblox\RobloxStudio";
const CRED_REG_PATH: &str = r"HKCU\Software\Wine\Credential Manager";
const CHANNEL_KEY: &str = r"HKCU\Software\ROBLOX Corporation\Environments\RobloxStudio\Channel";
const AUTH_NAME_PREFIX: &str = "Generic: https://www.roblox.com:RobloxStudioAuth";

pub fn err(msg: impl Into<String>) -> Error {
	msg.into().into()
}

/// What the core needs from the user interface. Every method may be
/// called from any thread.
pub trait Frontend: Send + Sync {
	/// Sets the bootstrapper status line.
	fn message(&self, msg: &str);
	/// Sets the bootstrapper detail line (deployment channel).
	fn info(&self, msg: &str);
	fn progress(&self, fraction: f64);
	/// Starts or stops the indeterminate progress animation; nests.
	fn pulse(&self, on: bool);
	fn show_bootstrapper(&self, show: bool);
	fn error(&self, msg: String);
	/// Makes Salt the browser login handler.
	fn set_mime(&self) -> Result;
	fn dark(&self) -> bool;
	/// Called when the Wine root changes outside the manager's editing.
	fn wine_root_changed(&self, path: &str);
}

pub struct Core {
	pub cfg: RwLock<Config>,
	pfx: RwLock<Prefix>,
	pub rbx: Arc<rbxweb::Client>,
	rpc: StudioRpc,
	fe: Arc<dyn Frontend>,
	/// Running Roblox Studio processes.
	pub count: AtomicUsize,
	/// A bootstrapper run is in progress.
	pub busy: AtomicBool,
	/// PID of the Studio process launched by this Salt, if any.
	pub child: AtomicU32,
	deployment: Mutex<Option<(Deployment, PathBuf)>>,
	/// Keeps the wineserver booted by setup running until Studio starts.
	hold: Mutex<Option<wine::Hold>>,
	watcher: Mutex<Option<Arc<AtomicBool>>>,
	weak: Weak<Core>,
}

/// Turns a stop flag into a guard that clears it on drop.
struct Pulse<'a>(&'a dyn Frontend, bool);

impl<'a> Pulse<'a> {
	fn new(fe: &'a dyn Frontend) -> Self {
		fe.pulse(true);
		Pulse(fe, true)
	}

	fn stop(&mut self) {
		if std::mem::take(&mut self.1) {
			self.0.pulse(false);
		}
	}
}

impl Drop for Pulse<'_> {
	fn drop(&mut self) {
		self.stop();
	}
}

impl Core {
	pub fn new(cfg: Config, fe: Arc<dyn Frontend>) -> Arc<Core> {
		let rbx = Arc::new(rbxweb::Client::new());
		Arc::new_cyclic(|weak| Core {
			pfx: RwLock::new(cfg.prefix()),
			cfg: RwLock::new(cfg),
			rpc: StudioRpc::spawn(rbx.clone()),
			rbx,
			fe,
			count: AtomicUsize::new(0),
			busy: AtomicBool::new(false),
			child: AtomicU32::new(0),
			deployment: Mutex::new(None),
			hold: Mutex::new(None),
			watcher: Mutex::new(None),
			weak: weak.clone(),
		})
	}

	pub fn pfx(&self) -> Prefix {
		self.pfx.read().unwrap().clone()
	}

	pub fn cfg(&self) -> Config {
		self.cfg.read().unwrap().clone()
	}

	fn message(&self, msg: &str) {
		self.fe.message(msg);
	}

	/// Rebuilds the prefix from the configuration.
	pub fn apply_config(&self) {
		let cfg = self.cfg();
		logging::set_debug(cfg.debug);
		self.rbx.set_debug(cfg.debug);

		let mut new = cfg.prefix();
		{
			let old = self.pfx.read().unwrap();
			// https://github.com/vinegarhq/vinegar/issues/746
			if old.root != new.root && old.running() {
				log::info!("Wine installation changed, killing Wine: {:?}", old.kill().err());
			}
		}
		let weak = self.weak.clone();
		new.sink = Some(Arc::new(move |line: &[u8]| {
			if let Some(core) = weak.upgrade() {
				core.wine_output(line);
			}
		}));
		*self.pfx.write().unwrap() = new;

		if Path::new(&cfg.studio.wineroot) == DIRS.wine_path {
			return;
		}
		// The user switched to another Wine; drop the unused download.
		if let Ok(path) = std::fs::canonicalize(&DIRS.wine_path) {
			log::info!("Removing unused Wine build path={}", path.display());
			let _ = std::fs::remove_dir_all(&path);
			let _ = std::fs::remove_file(&DIRS.wine_path);
		}
	}

	fn wine_output(&self, line: &[u8]) {
		let line = String::from_utf8_lossy(line);
		// Roblox log lines arrive through warn:seh in "debugger" mode.
		if line.len() >= 14 && line.get(10..13) == Some("seh") {
			if let Some(msg) = robloxlog::from_seh(&line) {
				if msg.len() >= 5 {
					robloxlog::handle(self, msg);
				}
			}
			return;
		}
		if line.contains("to unimplemented function advapi32.dll.SystemFunction036") {
			let _ = self.pfx().server(&[wine::server::KILL, "9"]);
			self.fe
				.error(l("Your Wineprefix is corrupt! Please delete all data in Salt's settings."));
		}
		logging::write(Level::Wine, &line);
	}

	// ---- Wine ----------------------------------------------------------

	/// Prepares the prefix, downloading Wine if needed. Reports whether
	/// the prefix was newly initialized.
	pub fn prepare_wine(&self) -> Result<bool> {
		let pfx = self.pfx();
		let first_run = !pfx.exists();
		log::info!("Setting up Wine first-time={first_run}");
		self.message(&l("Setting up Wine"));

		let cfg = self.cfg();
		if Path::new(&cfg.studio.wineroot) == DIRS.wine_path && pfx.wine_path().is_err() {
			self.update_wine("Latest").map_err(|e| err(format!("dl: {e}")))?;
		}
		if INFO.ntsync {
			log::info!("ntsync available");
		} else {
			log::warn!("/dev/ntsync is unavailable; Wine will synchronize threads through wineserver, which is slower");
		}
		if pfx.running() {
			self.hold_server(&pfx);
			return Ok(false);
		}

		pfx.prepare()?;
		self.update_wine_theme(self.fe.dark(), None);

		// Only for new prefixes, to not discard an existing appdata.
		if !first_run {
			self.hold_server(&pfx);
			return Ok(false);
		}
		let mut folders = wine::Key::with_path(r"HKCU\");
		folders
			.add(r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders")
			.set("Local AppData", dirs::windows(&DIRS.appdata_path))
			.set("Documents", dirs::windows(&user_dir("DOCUMENTS", "Documents")))
			.set("My Pictures", dirs::windows(&user_dir("PICTURES", "Pictures")));
		self.message(&l("Updating Paths"));
		pfx.registry_import_key(&folders).map_err(|e| err(format!("paths: {e}")))?;

		// Restart wineserver for wineboot to update app data internally.
		pfx.boot(&[wine::boot::RESTART]).run().map_err(|e| err(format!("restart: {e}")))?;
		self.restore_settings().map_err(|e| err(format!("restore: {e}")))?;
		self.hold_server(&pfx);
		Ok(true)
	}

	/// Boots the prefix now, alongside the update check, and keeps it
	/// running so that Studio doesn't wait for a boot of its own.
	fn hold_server(&self, pfx: &Prefix) {
		match pfx.hold() {
			Ok(h) => *self.hold.lock().unwrap() = Some(h),
			Err(e) => log::warn!("Failed to keep the wineserver running: {e}"),
		}
	}

	/// Installs a Kombucha Wine release ("Latest" or a tag).
	pub fn update_wine(&self, needle: &str) -> Result {
		let release = netutil::kombucha_release(needle).map_err(|e| err(format!("release: {e}")))?;
		let tag = &release.tag_name;
		log::debug!("Kombucha release tag={tag} published={:?}", release.published_at);
		let name = format!("{TAG_PREFIX}{tag}");
		let dir = DIRS.data.join(&name);

		if let Ok(path) = std::fs::canonicalize(&DIRS.wine_path) {
			if path.file_name() == Some(name.as_ref()) {
				log::info!("Wine build up to date tag={tag}");
				return Ok(());
			}
			log::info!("Removing old Wine build name={}", path.display());
			std::fs::remove_dir_all(&path).map_err(|e| err(format!("remove build: {e}")))?;
		}
		let _ = std::fs::remove_file(&DIRS.wine_path);

		if !dir.join("bin").exists() {
			let [asset] = release.assets.as_slice() else {
				return Err(err("expected single .tar.xz release"));
			};
			if asset.content_type != "application/x-xz" {
				return Err(err("expected single .tar.xz release"));
			}
			log::info!("Fetching Wine build tag={tag}");
			netutil::extract_tar_xz(&asset.browser_download_url, &DIRS.data)?;
		}
		std::os::unix::fs::symlink(&name, &DIRS.wine_path).map_err(|e| err(format!("create link: {e}")))?;
		log::info!("Updated Wine build configuration tag={tag}");

		let wine_path = DIRS.wine_path.display().to_string();
		if self.cfg.read().unwrap().studio.wineroot != wine_path {
			self.cfg.write().unwrap().studio.wineroot = wine_path.clone();
			self.apply_config();
			self.cfg.read().unwrap().save()?;
			self.fe.wine_root_changed(&wine_path);
		}
		Ok(())
	}

	/// Mirrors the desktop's light/dark preference into Wine's theme.
	///
	/// Skipped when the prefix's registry already matches, which avoids
	/// spawning regedit (and Studio's refresh flicker) on every launch.
	pub fn update_wine_theme(&self, dark: bool, offline: Option<&Registry>) {
		let pfx = self.pfx();
		let light = u32::from(!dark);
		let loaded;
		let offline = match offline {
			Some(r) => Some(r),
			None => {
				loaded = pfx.registry().ok();
				loaded.as_ref()
			}
		};
		let current = offline
			.and_then(|r| r.query(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"))
			.and_then(|k| k.value("AppsUseLightTheme"))
			.map(|v| v.data.clone());
		if current == Some(wine::Data::Dword(light)) {
			log::debug!("Wine theme already up to date dark={dark}");
			return;
		}

		let mut root = wine::Key::named("HKEY_CURRENT_USER");
		{
			let mgr = root.add(r"Software\Microsoft\Windows\CurrentVersion\ThemeManager");
			for v in ["ColorName", "DllName", "LoadedBefore", "SizeName"] {
				mgr.set(v, wine::Data::Delete);
			}
			// Reloading an msstyle needs the Windows API, so only switch
			// to no theme and change the colors.
			mgr.set("LoadedBefore", "0").set("ThemeActive", "0");
		}
		root.add(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
			.set("AppsUseLightTheme", light)
			.set("SystemUsesLightTheme", light);
		let colors = root.add(r"Control Panel\Colors");
		for (k, v) in if dark { DARK_THEME } else { LIGHT_THEME } {
			colors.set(*k, *v);
		}

		log::debug!("Changing Wine's theme dark={dark}");
		if let Err(e) = pfx.registry_import_key(&root) {
			log::error!("Failed to change Wine's theme: {e}");
		}
		// Registry edits fire no events; briefly running a GUI program
		// makes a running Studio pick up the new colors.
		if self.count.load(Ordering::Relaxed) > 0 {
			let _ = pfx.wine("start", ["cmd", "/c", "exit"]).run();
		}
	}

	// ---- Settings backup ----------------------------------------------

	fn backup_path() -> PathBuf {
		DIRS.data.join("settings.reg")
	}

	pub fn restore_settings(&self) -> Result {
		let pfx = self.pfx();
		let backup = Self::backup_path();
		if !pfx.exists() || !backup.exists() {
			return Ok(());
		}
		log::info!("Restoring Studio settings");
		pfx.registry_import_file(&backup)?;
		Ok(())
	}

	/// Saves Studio's registry settings and credentials, so they survive
	/// deleting the prefix.
	pub fn backup_settings(&self) -> Result {
		let pfx = self.pfx();
		if !pfx.running() {
			return Ok(());
		}
		// Studio keeps its layout as multi-line XML in the registry, which
		// `reg query` can't express; read the registry files instead.
		let r = pfx.registry().map_err(|e| err(format!("query: {e}")))?;
		let Some(studio) = r.query(STUDIO_REG_PATH) else {
			log::warn!("Nothing to backup!");
			return Ok(());
		};
		let mut stripped = wine::Key::with_path(r"HKCU\");
		stripped.add_key(r"Software\Roblox\RobloxStudio", studio);
		if let Some(cred) = r.query(CRED_REG_PATH) {
			stripped.add_key(r"Software\Wine\Credential Manager", cred);
		}
		let tmp = Self::backup_path().with_extension("reg.tmp");
		std::fs::write(&tmp, stripped.export_bytes())?;
		std::fs::rename(tmp, Self::backup_path())?;
		log::debug!("Backed up Studio Registry");
		Ok(())
	}

	// ---- Bootstrapper -------------------------------------------------

	/// Reads the logged-in user's .ROBLOSECURITY from Wine's credential
	/// manager, for private deployment channels.
	fn get_security(&self, offline: &Registry) -> Result {
		self.message(&l("Acquiring user authentication"));
		let cred = offline.query(CRED_REG_PATH).ok_or_else(|| err("credential manager missing"))?;
		let key = cred
			.value("EncryptionKey")
			.and_then(|v| v.data.as_bytes())
			.ok_or_else(|| err("encryption key missing"))?;
		let decrypt = |name: &str| -> Option<String> {
			let data = cred.query(name)?.value("Password")?.data.as_bytes()?;
			Some(String::from_utf8_lossy(&rc4(key, data)).into_owned())
		};
		let user = decrypt(&format!("{AUTH_NAME_PREFIX}userid")).ok_or_else(|| err("no current user"))?;
		log::info!("Using user for authentication user={user}");
		match decrypt(&format!("{AUTH_NAME_PREFIX}.ROBLOSECURITY{user}")) {
			Some(sec) => self.rbx.set_security(sec),
			None => log::warn!("ROBLOSECURITY cookie not found user={user}"),
		}
		Ok(())
	}

	fn webview_version(offline: Option<&Registry>) -> String {
		let Some(k) = offline.and_then(|r| r.query(wine::webview2::VERSION_PATH)) else {
			return String::new();
		};
		match k.value("DisplayVersion").and_then(|v| v.data.as_str()) {
			Some(v) => v.to_owned(),
			None => {
				log::warn!("WebView2 installed status missing, assuming uninstalled");
				String::new()
			}
		}
	}

	fn webview_installer(&self) -> PathBuf {
		DIRS.cache.join(format!("webview-{}.exe", self.cfg.read().unwrap().studio.webview))
	}

	/// Runs the bootstrapper: set up everything, then launch Studio.
	pub fn run(&self, args: &[String]) -> Result {
		if self.busy.swap(true, Ordering::SeqCst) {
			log::warn!("Bootstrapper currently in setup, ignoring run request");
			return Ok(());
		}
		struct Busy<'a>(&'a AtomicBool);
		impl Drop for Busy<'_> {
			fn drop(&mut self) {
				self.0.store(false, Ordering::SeqCst);
			}
		}
		let busy = Busy(&self.busy);

		self.fe.show_bootstrapper(true);
		let r = self.setup().map_err(|e| err(format!("setup: {e}")));
		drop(busy);
		if let Err(e) = r {
			self.hold.lock().unwrap().take();
			self.fe.show_bootstrapper(false);
			return Err(e);
		}
		let r = self.execute(args);
		self.hold.lock().unwrap().take();
		self.fe.show_bootstrapper(false);
		r
	}

	/// Prepares Studio, the prefix, WebView2 and DXVK.
	///
	/// Network work and prefix preparation are independent, so they run
	/// concurrently: on a first run or after a Wine update, wineboot no
	/// longer waits for hundreds of megabytes of downloads to finish.
	fn setup(&self) -> Result {
		if self.count.load(Ordering::Relaxed) > 0 {
			log::info!("Skipping setup!");
			return Ok(());
		}
		let pfx = self.pfx();
		// Without registry files, the prefix isn't initialized: no
		// WebView, no DXVK, no credentials.
		let offline = match pfx.registry() {
			Ok(r) => Some(r),
			Err(wine::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
			Err(e) => return Err(err(format!("registry: {e}"))),
		};
		let webview = Self::webview_version(offline.as_ref());
		if let Some(r) = &offline
			&& let Err(e) = self.get_security(r)
		{
			log::warn!("Retrieving authenticated user failed: {e}");
		}

		let cfg = self.cfg();
		let (deploy, wine, web, dxvk) = std::thread::scope(|s| {
			let wine = s.spawn(|| self.prepare_wine());
			let web = s.spawn(|| self.download_webview(&webview, &cfg));
			let dxvk = s.spawn(|| self.download_dxvk(&cfg));
			let deploy = self.update_deployment(&cfg);
			(deploy, wine.join(), web.join(), dxvk.join())
		});
		let join = |r: std::thread::Result<Result>| r.unwrap_or_else(|_| Err(err("worker panicked")));
		deploy?;
		join(wine.map(|r| r.map(drop)))?;
		join(web).map_err(|e| err(format!("download webview: {e}")))?;
		join(dxvk).map_err(|e| err(format!("download dxvk: {e}")))?;

		let dir = self.version_dir().ok_or_else(|| err("no deployment"))?;
		self.copy_overlay(&dir).map_err(|e| err(format!("overlay: {e}")))?;
		self.apply_fflags(&cfg, &dir).map_err(|e| err(format!("fflags: {e}")))?;
		self.write_channel()?;
		self.install_webview(&webview, &cfg)
			.map_err(|e| err(format!("install webview: {e}")))?;
		// DXVK runs no program, which would leave the wineserver to its
		// persistence timeout; install it right before Studio runs.
		self.setup_dxvk(&cfg, &dir).map_err(|e| err(format!("dxvk: {e}")))?;
		Ok(())
	}

	pub fn version_dir(&self) -> Option<PathBuf> {
		self.deployment.lock().unwrap().as_ref().map(|(_, d)| d.clone())
	}

	fn update_deployment(&self, cfg: &Config) -> Result {
		let mut pulse = Pulse::new(&*self.fe);
		self.message(&l("Checking for updates"));
		let bin = if cfg.studio.forced_version.is_empty() {
			rbxbin::get_deployment(&self.rbx, STUDIO, &cfg.studio.channel).map_err(|e| err(format!("fetch: {e}")))?
		} else {
			Deployment {
				kind: STUDIO,
				channel: cfg.studio.channel.clone(),
				guid: cfg.studio.forced_version.clone(),
				client: None,
			}
		};
		self.fe.info(&bin.channel);
		let dir = DIRS.versions.join(&bin.guid);
		log::info!("Using Deployment guid={} channel={}", bin.guid, bin.channel);
		*self.deployment.lock().unwrap() = Some((bin.clone(), dir.clone()));

		// The executable is installed last, so a missing or corrupt one
		// means the installation is incomplete.
		match wine::pe::validate(&dir.join("RobloxStudioBeta.exe")) {
			Ok(()) => {
				self.message(&l("Up to date"));
				log::info!("Up to date guid={}", bin.guid);
				return Ok(());
			}
			Err(e) => log::info!("Installing Studio new={} reason={e}", bin.guid),
		}
		self.message(&l("Installing Studio"));
		remove_unique_files(&DIRS.versions, &[bin.guid.as_str()]);

		self.message(&l("Finding Mirror"));
		let mirror = rbxbin::Mirror::get().map_err(|e| err(format!("fetch mirror: {e}")))?;
		self.message(&l("Fetching Package List"));
		let (pkgs, pdirs) = std::thread::scope(|s| {
			let pkgs = s.spawn(|| mirror.packages(&bin));
			(pkgs.join(), mirror.binary_directories(&bin))
		});
		let pkgs = pkgs
			.map_err(|_| err("worker panicked"))?
			.map_err(|e| err(format!("fetch packages: {e}")))?;
		let pdirs = pdirs.map_err(|e| err(format!("fetch package dirs: {e}")))?;
		let sums: Vec<&str> = pkgs.iter().map(|p| p.checksum.as_str()).collect();
		remove_unique_files(&DIRS.downloads, &sums);

		pulse.stop();
		self.message(&l("Installing Packages"));
		log::info!("Installing Packages count={} dir={}", pkgs.len(), dir.display());
		let cancel = AtomicBool::new(false);
		let fe = &*self.fe;
		let last = Mutex::new(0u32);
		if let Err(e) = rbxbin::install(
			&bin,
			&mirror,
			pkgs,
			&pdirs,
			&DIRS.downloads,
			&dir,
			8,
			&|f| {
				// Throttle UI updates to 0.5% steps.
				let step = (f * 200.0) as u32;
				let mut l = last.lock().unwrap();
				if step != *l {
					*l = step;
					fe.progress(f);
				}
			},
			&cancel,
		) {
			let _ = std::fs::remove_dir_all(&dir);
			return Err(e.into());
		}

		let _pulse = Pulse::new(&*self.fe);
		self.message(&l("Writing AppSettings"));
		rbxbin::write_app_settings(&dir).map_err(|e| err(format!("appsettings: {e}")))?;
		log::info!("Successfully installed! guid={}", bin.guid);
		Ok(())
	}

	/// Studio needs its channel in the registry to recognize it.
	/// https://github.com/vinegarhq/vinegar/issues/649
	fn write_channel(&self) -> Result {
		let channel = match &*self.deployment.lock().unwrap() {
			Some((d, _)) => d.channel.clone(),
			None => return Ok(()),
		};
		if channel.is_empty() || channel == "LIVE" {
			return Ok(());
		}
		self.message(&l("Writing Registry"));
		self.pfx()
			.registry_add(CHANNEL_KEY, "www.roblox.com", channel)
			.map_err(|e| err(format!("set channel reg: {e}")))
	}

	fn copy_overlay(&self, dir: &Path) -> Result {
		let src = DIRS.overlays.join(STUDIO.short().to_lowercase());
		if !src.exists() {
			return Ok(());
		}
		self.message(&l("Copying Overlay"));
		copy_dir(&src, dir)?;
		Ok(())
	}

	fn apply_fflags(&self, cfg: &Config, dir: &Path) -> Result {
		let mut f = cfg.studio.fflags.clone();
		let renderer = if cfg.studio.dxvk_version().is_some() {
			"D3D11"
		} else {
			cfg.studio.renderer.as_str()
		};
		const RENDERERS: [&str; 4] = ["D3D11", "Vulkan", "D3D11FL10", "OpenGL"];
		if !RENDERERS.contains(&renderer) {
			return Err(err(format!("unknown renderer: {renderer}")));
		}
		for r in RENDERERS {
			f.insert(format!("FFlagDebugGraphicsPrefer{r}"), rbxbin::FFlag::Bool(r == renderer));
			f.insert(format!("FFlagDebugGraphicsDisable{r}"), rbxbin::FFlag::Bool(r != renderer));
		}
		self.message(&l("Applying FFlags"));
		rbxbin::apply_fflags(&f, dir)?;
		Ok(())
	}

	fn download_webview(&self, installed: &str, cfg: &Config) -> Result {
		let want = &cfg.studio.webview;
		if want.is_empty() || installed == want {
			return Ok(());
		}
		let inst = self.webview_installer();
		if wine::pe::validate(&inst).is_ok() {
			return Ok(());
		}
		let _pulse = Pulse::new(&*self.fe);
		log::info!("Fetching WebView upload={want}");
		let d = wine::webview2::Channel::Stable
			.runtime(want, "x64")
			.map_err(|e| err(format!("fetch: {e}")))?;
		log::info!("Downloading WebView catalog={}", d.delivery.catalog_id);
		std::fs::create_dir_all(&DIRS.cache)?;
		netutil::download(&d.url, &inst, &|_| {})?;
		Ok(())
	}

	fn install_webview(&self, installed: &str, cfg: &Config) -> Result {
		let want = &cfg.studio.webview;
		let pfx = self.pfx();
		log::info!("Checking WebView against={want}");
		if !installed.is_empty() && installed != want {
			self.message(&l("Uninstalling WebView"));
			log::info!("Uninstalling WebView current={installed} new={want}");
			wine::webview2::uninstall(&pfx, installed).map_err(|e| err(format!("uninstall: {e}")))?;
		}
		if installed == want || want.is_empty() {
			return Ok(());
		}
		let inst = self.webview_installer();
		self.message(&l("Installing WebView"));
		let _pulse = Pulse::new(&*self.fe);
		wine::webview2::install(&pfx, &inst)?;
		Ok(())
	}

	fn dxvk_tarball(version: &str) -> PathBuf {
		DIRS.cache.join(format!("dxvk-{version}.tar.gz"))
	}

	fn download_dxvk(&self, cfg: &Config) -> Result {
		let Some(version) = cfg.studio.dxvk_version() else {
			return Ok(());
		};
		let name = Self::dxvk_tarball(version);
		if name.exists() {
			return Ok(());
		}
		std::fs::create_dir_all(&DIRS.cache)?;
		log::info!("Downloading DXVK version={version}");
		netutil::download(&wine::dxvk::url(version), &name, &|_| {})?;
		Ok(())
	}

	fn setup_dxvk(&self, cfg: &Config, dir: &Path) -> Result {
		let Some(version) = cfg.studio.dxvk_version() else {
			return Ok(());
		};
		log::info!("Checking DXVK against={version}");
		let pfx = self.pfx();
		// DXVK may be installed next to Studio, or (older Salt) in the
		// prefix itself; either works.
		let mut native = None;
		let installed = match wine::dxvk::dll_version(&dir.join("d3d11.dll")) {
			Ok(v) => v,
			Err(wine::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
				let v = wine::dxvk::version(&pfx).unwrap_or_default();
				native = Some(v.clone());
				v
			}
			Err(e) => return Err(err(format!("get version: {e}"))),
		};
		if wine::dxvk::same_version(&installed, version) {
			return Ok(());
		}
		self.message(&l("Extracting DXVK"));
		log::info!("Installing DXVK current={installed} new={version}");
		let _pulse = Pulse::new(&*self.fe);
		if native.is_some_and(|v| !v.is_empty()) {
			// Prefer the per-Studio installation; restoring Wine's DLLs
			// is optional as overrides prefer Studio's directory.
			log::info!("Restoring DirectX DLLs");
			let _ = wine::dxvk::restore(&pfx);
		}
		let tarball = Self::dxvk_tarball(version);
		if !tarball.exists() {
			self.download_dxvk(cfg)?;
		}
		wine::dxvk::extract_x64(std::fs::File::open(&tarball)?, dir)?;
		Ok(())
	}

	/// Builds the Studio command.
	pub fn command(&self, args: &[String]) -> Result<wine::Cmd> {
		let dir = self.version_dir().ok_or_else(|| err("no deployment"))?;
		let pfx = self.pfx();
		pfx.wine_path()?;
		let mut cmd = pfx.wine(dir.join("RobloxStudioBeta.exe"), args);

		// Authentication calls go to the running instance.
		if args.first().is_some_and(|a| a.starts_with("roblox-studio-auth:")) {
			return Ok(cmd);
		}
		let cfg = self.cfg();
		if !cfg.studio.desktop.is_empty() {
			cmd.insert_args(0, &["explorer".to_owned(), format!("/desktop={},{}", uuid(), cfg.studio.desktop)]);
		}
		let launcher: Vec<&str> = cfg.studio.launcher.split_whitespace().collect();
		if !launcher.is_empty() {
			let p = cfg
				.studio
				.launcher_path()
				.ok_or_else(|| err(format!("bad launcher: {} not found", launcher[0])))?;
			cmd.wrap(p, &launcher[1..]);
		}
		Ok(cmd)
	}

	fn execute(&self, args: &[String]) -> Result {
		let mut cmd = self.command(args)?;
		log::info!("Launching Studio cmd={cmd}");
		self.message(&l("Launching Studio"));

		let cfg = self.cfg();
		if cfg.studio.log_source == "file" {
			self.start_log_watcher();
		}

		let mut child = cmd.spawn()?;
		self.child.store(child.id(), Ordering::SeqCst);
		self.count.fetch_add(1, Ordering::SeqCst);
		// Studio connects to the wineserver within moments of starting.
		if let Some(hold) = self.hold.lock().unwrap().take() {
			std::thread::spawn(move || {
				std::thread::sleep(std::time::Duration::from_secs(10));
				drop(hold);
			});
		}
		self.fe.show_bootstrapper(false);

		if cfg.studio.gamemode
			&& let Err(e) = crate::gamemode::register(child.id())
		{
			log::error!("Failed to register with GameMode: {e}");
		}

		let status = child.inner.wait();
		self.child.store(0, Ordering::SeqCst);
		if self.count.fetch_sub(1, Ordering::SeqCst) == 1 {
			self.stop_log_watcher();
		}
		let status = status?;
		use std::os::unix::process::ExitStatusExt;
		if let Some(sig) = status.signal() {
			log::warn!("Roblox was killed! signal={sig}");
			return Ok(());
		}
		if !status.success() {
			return Err(err(format!("Studio exited with {status}")));
		}
		Ok(())
	}

	fn start_log_watcher(&self) {
		let mut w = self.watcher.lock().unwrap();
		if w.is_some() {
			return;
		}
		let pfx = self.pfx();
		let dirs = vec![
			DIRS.appdata_path.join("Roblox/logs"),
			pfx.appdata_dir().join("Local/Roblox/logs"),
		];
		let stop = Arc::new(AtomicBool::new(false));
		let ev: Arc<dyn LogEvents> = self.weak.upgrade().expect("core alive");
		robloxlog::watch(dirs, ev, stop.clone());
		*w = Some(stop);
	}

	fn stop_log_watcher(&self) {
		if let Some(stop) = self.watcher.lock().unwrap().take() {
			stop.store(true, Ordering::Relaxed);
		}
	}

	/// Kills the Studio process this Salt launched.
	pub fn kill_child(&self) {
		let pid = self.child.load(Ordering::SeqCst);
		if pid != 0 {
			log::debug!("Killing Roblox pid={pid}");
			// SAFETY: plain kill(2).
			unsafe { libc::kill(pid as i32, libc::SIGKILL) };
		}
	}

	// ---- Manager actions ----------------------------------------------

	pub fn kill_prefix(&self) -> Result {
		self.pfx().kill()?;
		Ok(())
	}

	pub fn delete_prefixes(&self) -> Result {
		let _ = self.pfx().kill();
		remove_all(&DIRS.prefixes)
	}

	pub fn delete_deployments(&self) -> Result {
		remove_all(&DIRS.versions)
	}

	pub fn clear_cache(&self) -> Result {
		let Ok(rd) = std::fs::read_dir(&DIRS.cache) else {
			return Ok(());
		};
		for e in rd.flatten() {
			let p = e.path();
			if p == DIRS.logs {
				continue;
			}
			log::info!("Removing cache file path={}", p.display());
			if p.is_dir() {
				std::fs::remove_dir_all(&p)?;
			} else {
				std::fs::remove_file(&p)?;
			}
		}
		Ok(())
	}
}

impl LogEvents for Core {
	fn debug(&self) -> bool {
		logging::debug_enabled()
	}

	fn rpc(&self, line: &str) {
		if self.cfg.read().unwrap().studio.discord_rpc {
			self.rpc.send(line);
		}
	}

	fn instance_started(&self) {
		self.count.fetch_add(1, Ordering::SeqCst);
	}

	fn instance_exited(&self) {
		// Only extra instances; the launched one is tracked by its process.
		if self.count.load(Ordering::SeqCst) > 1 {
			log::warn!("Extra studio instance closed!");
			self.count.fetch_sub(1, Ordering::SeqCst);
		}
	}

	fn browser_login_failed(&self) {
		// Make sure browser login works instead.
		if let Err(e) = self.fe.set_mime() {
			let _ = self.pfx().kill();
			self.fe.error(e.to_string());
		}
	}
}

fn user_dir(kind: &str, fallback: &str) -> PathBuf {
	// ~/.config/user-dirs.dirs: XDG_DOCUMENTS_DIR="$HOME/Documents"
	let home = dirs::home();
	let cfg = std::env::var_os("XDG_CONFIG_HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|| home.join(".config"));
	let key = format!("XDG_{kind}_DIR=");
	std::fs::read_to_string(cfg.join("user-dirs.dirs"))
		.ok()
		.and_then(|s| {
			s.lines().find_map(|l| {
				let v = l.strip_prefix(&key)?.trim_matches('"');
				Some(PathBuf::from(v.replace("$HOME", &home.display().to_string())))
			})
		})
		.filter(|p| p.is_absolute())
		.unwrap_or_else(|| home.join(fallback))
}

fn uuid() -> String {
	let mut b = [0u8; 16];
	if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
		use std::io::Read;
		let _ = f.read_exact(&mut b);
	}
	b[6] = (b[6] & 0x0f) | 0x40;
	b[8] = (b[8] & 0x3f) | 0x80;
	let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
	format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// RC4, as used by Wine's credential manager.
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
	let mut s: Vec<u8> = (0..=255).collect();
	let mut j = 0u8;
	for i in 0..256 {
		j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len().max(1)]);
		s.swap(i, j as usize);
	}
	let (mut i, mut j) = (0u8, 0u8);
	data.iter()
		.map(|b| {
			i = i.wrapping_add(1);
			j = j.wrapping_add(s[i as usize]);
			s.swap(i as usize, j as usize);
			b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
		})
		.collect()
}

fn remove_all(p: &Path) -> Result {
	match std::fs::remove_dir_all(p) {
		Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
		_ => Ok(()),
	}
}

/// Removes every entry of `dir` not named in `keep`.
fn remove_unique_files(dir: &Path, keep: &[&str]) {
	let rd = match std::fs::read_dir(dir) {
		Ok(rd) => rd,
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
		Err(e) => {
			log::error!("Failed to cleanup directory dir={} err={e}", dir.display());
			return;
		}
	};
	for e in rd.flatten() {
		let name = e.file_name();
		if keep.iter().any(|k| name == **k) {
			continue;
		}
		log::info!("Removing unique file dir={} file={}", dir.display(), name.to_string_lossy());
		let p = e.path();
		let r = if p.is_dir() {
			std::fs::remove_dir_all(&p)
		} else {
			std::fs::remove_file(&p)
		};
		if let Err(e) = r {
			log::error!("Failed to cleanup file: {e}");
			break;
		}
	}
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
	std::fs::create_dir_all(dst)?;
	for e in std::fs::read_dir(src)? {
		let e = e?;
		let to = dst.join(e.file_name());
		if e.file_type()?.is_dir() {
			copy_dir(&e.path(), &to)?;
		} else {
			std::fs::copy(e.path(), to)?;
		}
	}
	Ok(())
}

const DARK_THEME: &[(&str, &str)] = &[
	("ActiveBorder", "34 34 38"),
	("ActiveTitle", "46 46 50"),
	("AppWorkSpace", "45 45 49"),
	("Background", "29 29 32"),
	("ButtonAlternativeFace", "98 160 234"),
	("ButtonDkShadow", "22 22 25"),
	("ButtonFace", "46 46 50"),
	("ButtonHilight", "57 57 61"),
	("ButtonLight", "53 53 57"),
	("ButtonShadow", "25 25 28"),
	("ButtonText", "255 255 255"),
	("GradientActiveTitle", "46 46 50"),
	("GradientInactiveTitle", "40 40 44"),
	("GrayText", "136 136 136"),
	("Hilight", "98 160 234"),
	("HilightText", "255 255 255"),
	("InactiveBorder", "40 40 44"),
	("InactiveTitle", "40 40 44"),
	("InactiveTitleText", "211 211 211"),
	("InfoText", "255 255 255"),
	("InfoWindow", "54 54 58"),
	("Menu", "54 54 58"),
	("MenuBar", "46 46 50"),
	("MenuHilight", "98 160 234"),
	("MenuText", "255 255 255"),
	("Scrollbar", "46 46 50"),
	("TitleText", "255 255 255"),
	("Window", "29 29 32"),
	("WindowFrame", "46 46 50"),
	("WindowText", "255 255 255"),
];

const LIGHT_THEME: &[(&str, &str)] = &[
	("ActiveBorder", "255 255 255"),
	("ActiveTitle", "50 150 250"),
	("AppWorkSpace", "128 128 128"),
	("Background", "37 111 149"),
	("ButtonAlternateFace", "255 255 255"),
	("ButtonAlternativeFace", "98 160 234"),
	("ButtonDkShadow", "106 106 106"),
	("ButtonFace", "245 245 245"),
	("ButtonHilight", "255 255 255"),
	("ButtonLight", "227 227 227"),
	("ButtonShadow", "166 166 166"),
	("ButtonText", "0 0 0"),
	("GradientActiveTitle", "50 150 250"),
	("GradientInactiveTitle", "128 128 128"),
	("GrayText", "106 106 106"),
	("Hilight", "48 150 250"),
	("HilightText", "255 255 255"),
	("HotTrackingColor", "48 150 250"),
	("InactiveBorder", "255 255 255"),
	("InactiveTitle", "128 128 128"),
	("InactiveTitleText", "200 200 200"),
	("InfoText", "0 0 0"),
	("InfoWindow", "255 255 255"),
	("Menu", "255 255 255"),
	("MenuBar", "255 255 255"),
	("MenuHilight", "48 150 250"),
	("MenuText", "0 0 0"),
	("Scrollbar", "255 255 255"),
	("TitleText", "0 0 0"),
	("Window", "255 255 255"),
	("WindowFrame", "158 158 158"),
	("WindowText", "0 0 0"),
];

#[cfg(test)]
mod tests {
	#[test]
	fn rc4_vector() {
		// RFC 6229 style known answer: key "Key", plaintext "Plaintext".
		let out = super::rc4(b"Key", b"Plaintext");
		assert_eq!(out, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
	}
}
