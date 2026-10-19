use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use super::resource;

/// The bootstrapper window. Main thread only.
pub struct Bootstrapper {
	pub win: adw::Window,
	pub logo: gtk::Image,
	pub status: gtk::Label,
	pub info: gtk::Label,
	pub progress: gtk::ProgressBar,
	pulses: Cell<u32>,
	source: RefCell<Option<glib::SourceId>>,
	last_fraction: Rc<Cell<Option<Instant>>>,
}

impl Bootstrapper {
	pub fn new() -> Self {
		let b = gtk::Builder::from_resource(&resource("ui/bootstrapper.ui"));
		let win: adw::Window = b.object("window").expect("window");
		win.connect_close_request(|w| {
			// Cancel: there is no way to interrupt setup midway, so quit.
			if let Some(app) = w.application() {
				app.quit();
			}
			glib::Propagation::Proceed
		});
		Bootstrapper {
			win,
			logo: b.object("logo").expect("logo"),
			status: b.object("status").expect("status"),
			info: b.object("info").expect("info"),
			progress: b.object("progress").expect("progress"),
			pulses: Cell::new(0),
			source: RefCell::new(None),
			last_fraction: Rc::new(Cell::new(None)),
		}
	}

	pub fn show(&self, app: &adw::Application, show: bool) {
		if show {
			if self.win.application().is_none() {
				app.add_window(&self.win);
			}
			self.win.present();
		} else {
			self.win.set_visible(false);
			if self.win.application().is_some() {
				app.remove_window(&self.win);
			}
		}
	}

	pub fn set_fraction(&self, f: f64) {
		self.last_fraction.set(Some(Instant::now()));
		self.progress.set_fraction(f.clamp(0.0, 1.0));
	}

	/// Starts or stops the activity animation. Calls nest, and a
	/// determinate progress set within the last second takes precedence,
	/// since setup now downloads and prepares Wine at the same time.
	pub fn pulse(&self, on: bool) {
		let n = if on {
			self.pulses.get() + 1
		} else {
			self.pulses.get().saturating_sub(1)
		};
		self.pulses.set(n);
		if n > 0 && self.source.borrow().is_none() {
			let bar = self.progress.clone();
			let last = self.last_fraction.clone();
			let id = glib::timeout_add_local(Duration::from_millis(128), move || {
				if last.get().is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
					bar.pulse();
				}
				glib::ControlFlow::Continue
			});
			*self.source.borrow_mut() = Some(id);
		} else if n == 0
			&& let Some(id) = self.source.borrow_mut().take()
		{
			id.remove();
		}
	}
}
