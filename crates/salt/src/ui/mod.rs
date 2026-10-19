//! GTK/libadwaita user interface.
//!
//! Widgets live on the main thread. Worker threads reach them through
//! [`ui`], which queues a closure on the main loop, the equivalent of
//! the Go version's gutil.IdleAdd.

mod bootstrapper;
mod manager;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config::Config;
use crate::core::{Core, Frontend, Result, err};
use crate::dirs::DIRS;
use crate::i18n::l;
use crate::logging;
use crate::sysinfo::INFO;

/// Inside the Salt flatpak, the sandbox only lets Salt own D-Bus names
/// under the flatpak's app ID; using it exactly also routes Studio's
/// browser-login callbacks (opened through Salt's desktop file) to Salt.
pub const APP_ID: &str = match option_env!("SALT_APP_ID") {
	Some(id) => id,
	None => "io.github.glockerz.Salt",
};
const RESOURCE: &str = "/io/github/glockerz/Salt";

pub fn resource(path: &str) -> String {
	format!("{RESOURCE}/{path}")
}

/// Main-thread application state.
pub struct Ui {
	pub app: adw::Application,
	pub core: Arc<Core>,
	pub boot: bootstrapper::Bootstrapper,
	pub mgr: RefCell<Option<Rc<manager::Manager>>>,
	holds: RefCell<Vec<gio::ApplicationHoldGuard>>,
	version: String,
}

thread_local! {
	static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// Runs `f` with the UI; main thread only.
pub fn local(f: impl FnOnce(&Rc<Ui>)) {
	if let Some(u) = UI.with_borrow(|u| u.clone()) {
		f(&u);
	}
}

/// Runs `f` with the UI on the main thread.
pub fn ui(f: impl FnOnce(&Rc<Ui>) + Send + 'static) {
	glib::MainContext::default().invoke(move || {
		let u = UI.with_borrow(|u| u.clone());
		if let Some(u) = u {
			f(&u);
		}
	});
}

impl Ui {
	fn hold(&self) {
		self.holds.borrow_mut().push(self.app.hold());
	}

	fn release(&self) {
		self.holds.borrow_mut().pop();
	}

	/// Runs `f` on a worker thread, showing its error, if any.
	pub fn spawn(f: impl FnOnce() -> Result + Send + 'static) {
		std::thread::spawn(move || {
			let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
				.unwrap_or_else(|p| Err(err(format!("internal error: {}", panic_msg(&p)))));
			if let Err(e) = r {
				let msg = e.to_string();
				ui(move |u| u.show_error(&msg));
			}
		});
	}

	pub fn show_error(&self, msg: &str) {
		log::error!("Error! err={msg}");
		logging::flush();
		// When the bootstrapper fails, its window goes away and the
		// dialog would be the last thing keeping the app alive.
		self.hold();
		let d = adw::AlertDialog::new(Some(&l("Something went wrong")), Some(msg));
		d.add_responses(&[("okay", &l("Ok")), ("open", &l("Open Log"))]);
		d.set_close_response("okay");
		d.set_default_response(Some("okay"));
		d.set_response_appearance("open", adw::ResponseAppearance::Suggested);
		let parent = self.app.active_window();
		d.choose(parent.as_ref(), gio::Cancellable::NONE, |r| {
			if r == "open" {
				let uri = glib::filename_to_uri(&*logging::PATH, None).map(|u| u.to_string()).unwrap_or_default();
				gtk::UriLauncher::new(&uri).launch(gtk::Window::NONE, gio::Cancellable::NONE, |_| {});
			}
			ui(|u| u.release());
		});
	}

	fn command_line(self: &Rc<Self>, cl: &gio::ApplicationCommandLine) -> glib::ExitCode {
		let mut args: Vec<String> = cl
			.arguments()
			.iter()
			.skip(1)
			.map(|a| a.to_string_lossy().into_owned())
			.collect();
		if args.first().is_some_and(|a| a == "run") {
			args.remove(0);
		}

		let fresh = !DIRS.data.exists();
		if fresh && let Err(e) = std::fs::create_dir_all(&DIRS.data) {
			log::error!("Failed to initialize data directory: {e}");
			return glib::ExitCode::FAILURE;
		}

		if fresh || (args.len() == 1 && (args[0] == "manage" || args[0] == "config")) {
			let mgr = self
				.mgr
				.borrow_mut()
				.get_or_insert_with(|| manager::Manager::new(self))
				.clone();
			mgr.win.present();
			// Entry rows otherwise pull the page down to the last one.
			let page = mgr.page.clone();
			glib::idle_add_local_once(move || page.scroll_to_top());
			if fresh {
				mgr.navigation.push_by_tag("welcome");
			}
			return glib::ExitCode::SUCCESS;
		}

		// The bootstrapper adds its window from a thread, possibly after
		// GApplication would have quit for having no windows.
		self.hold();
		let core = self.core.clone();
		std::thread::spawn(move || {
			let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| core.run(&args)))
				.unwrap_or_else(|p| Err(err(format!("internal error: {}", panic_msg(&p)))));
			ui(move |u| {
				if let Err(e) = r {
					u.show_error(&e.to_string());
				}
				u.release();
			});
		});
		glib::ExitCode::SUCCESS
	}

	fn shutdown(&self) {
		if let Err(e) = self.core.backup_settings() {
			log::error!("Failed to backup Studio settings: {e}");
		}
		if self.core.count.load(std::sync::atomic::Ordering::Relaxed) > 1 {
			log::warn!("Handing off Wineserver control!");
		} else {
			log::info!("Goodbye!");
		}
		logging::flush();
	}

	/// Sets Salt as the handler for Studio's browser login.
	fn set_mime(&self) -> Result {
		let app = gio::AppInfo::all()
			.into_iter()
			.find(|a| a.id().is_some_and(|id| id.starts_with(APP_ID)))
			.ok_or_else(|| err("Where is Salt's desktop file? Is this a proper installation?"))?;
		log::info!("Setting as default application for browser login");
		app.set_as_default_for_type("x-scheme-handler/roblox-studio-auth")
			.map_err(|e| err(format!("browser login set: {e}")))
	}
}

fn panic_msg(p: &Box<dyn std::any::Any + Send>) -> String {
	p.downcast_ref::<&str>()
		.map(|s| s.to_string())
		.or_else(|| p.downcast_ref::<String>().cloned())
		.unwrap_or_else(|| "panic".into())
}

/// The core's view of the UI.
struct GtkFrontend;

impl Frontend for GtkFrontend {
	fn message(&self, msg: &str) {
		log::info!("{msg}");
		let msg = msg.to_owned();
		ui(move |u| u.boot.status.set_label(&msg));
	}

	fn info(&self, msg: &str) {
		let msg = msg.to_owned();
		ui(move |u| u.boot.info.set_label(&msg));
	}

	fn progress(&self, f: f64) {
		ui(move |u| u.boot.set_fraction(f));
	}

	fn pulse(&self, on: bool) {
		ui(move |u| u.boot.pulse(on));
	}

	fn show_bootstrapper(&self, show: bool) {
		ui(move |u| u.boot.show(&u.app, show));
	}

	fn error(&self, msg: String) {
		ui(move |u| u.show_error(&msg));
	}

	fn set_mime(&self) -> Result {
		let (tx, rx) = std::sync::mpsc::channel();
		ui(move |u| {
			let _ = tx.send(u.set_mime().map_err(|e| e.to_string()));
		});
		rx.recv().map_err(|_| err("UI unavailable"))?.map_err(err)
	}

	fn dark(&self) -> bool {
		let (tx, rx) = std::sync::mpsc::channel();
		ui(move |u| {
			let _ = tx.send(u.app.style_manager().is_dark());
		});
		rx.recv().unwrap_or(false)
	}

	fn wine_root_changed(&self, path: &str) {
		let path = path.to_owned();
		ui(move |u| {
			if let Some(m) = &*u.mgr.borrow() {
				m.wine_row.set_subtitle(&path);
			}
		});
	}
}

fn app_version() -> String {
	gio::resources_lookup_data(&resource("metainfo.xml"), gio::ResourceLookupFlags::NONE)
		.ok()
		.and_then(|b| {
			let s = String::from_utf8_lossy(&b).into_owned();
			let i = s.find("<release ")?;
			let rest = &s[i..];
			let v = rest.find("version=\"")? + 9;
			let end = rest[v..].find('"')?;
			Some(rest[v..v + end].to_owned())
		})
		.unwrap_or_else(|| env!("CARGO_PKG_VERSION").into())
}

pub fn run() -> glib::ExitCode {
	gio::resources_register_include!("salt.gresource").expect("register resources");

	let app = adw::Application::new(Some(APP_ID), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
	glib::set_application_name("Salt");

	app.connect_startup(|app| {
		if let Some(display) = gtk::gdk::Display::default() {
			gtk::IconTheme::for_display(&display).add_resource_path(&resource("icons"));
		}
		gtk::Window::set_default_icon_name("io.github.glockerz.Salt");
		log::info!(
			"System information cpu={:?} mem={} distro={:?} display={:?} ntsync={}",
			INFO.cpu,
			format!("{:.1} GiB", INFO.memory as f64 / f64::from(1u32 << 30)),
			INFO.distro,
			INFO.display,
			INFO.ntsync
		);
		log::info!(
			"DRM Devices cards={:?}",
			INFO.cards.iter().map(|c| c.to_string()).collect::<Vec<_>>()
		);

		let (cfg, cfg_err) = match Config::load() {
			Ok(c) => (c, None),
			Err(e) => (Config::default(), Some(format!("config error: {e}"))),
		};
		let core = Core::new(cfg, Arc::new(GtkFrontend));
		core.apply_config();
		crate::winehost::serve(core.clone());

		let u = Rc::new(Ui {
			app: app.clone(),
			boot: bootstrapper::Bootstrapper::new(),
			core,
			mgr: RefCell::new(None),
			holds: RefCell::new(Vec::new()),
			version: app_version(),
		});
		UI.with_borrow_mut(|slot| *slot = Some(u.clone()));

		if let Some(e) = cfg_err {
			u.show_error(&e);
		}
		// ChromeOS gives Crostini about 4 GB.
		if INFO.memory < 3 << 30 && !u.core.cfg().debug {
			u.show_error(&l(
				"This system does not meet the minimum requirements to run Roblox Studio. \
				 It is recommended to run Salt with sufficient memory and graphics.",
			));
		}

		let core = u.core.clone();
		app.style_manager().connect_dark_notify(move |sm| {
			let dark = sm.is_dark();
			let core = core.clone();
			// regedit takes a moment; keep the UI responsive.
			std::thread::spawn(move || core.update_wine_theme(dark, None));
		});

	});

	app.connect_command_line(|_, cl| {
		UI.with_borrow(|u| u.clone())
			.map(|u| u.command_line(cl))
			.unwrap_or(glib::ExitCode::FAILURE)
	});

	app.connect_shutdown(|_| {
		if let Some(u) = UI.with_borrow(|u| u.clone()) {
			u.shutdown();
		}
	});

	let args: Vec<String> = std::env::args().collect();
	app.run_with_args(&args)
}
