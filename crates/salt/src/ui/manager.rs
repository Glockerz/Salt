//! The manager (settings) window.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::{Ui, resource, ui};
use crate::config::{self, Config};
use crate::core::{Core, Result};
use crate::dirs::{DIRS, TAG_PREFIX};
use crate::i18n::l;
use crate::sysinfo::INFO;

pub struct Manager {
	pub win: adw::ApplicationWindow,
	pub navigation: adw::NavigationView,
	pub page: adw::PreferencesPage,
	pub wine_row: adw::ActionRow,
	overlay: adw::ToastOverlay,
	tags: gtk::StringList,
	spinner: RefCell<Option<adw::Spinner>>,
}

fn obj<T: IsA<glib::Object>>(b: &gtk::Builder, name: &str) -> T {
	b.object(name).unwrap_or_else(|| panic!("manager.ui: missing {name}"))
}

/// Applies and saves the configuration.
fn save(core: &Core) {
	if let Err(e) = core.cfg().validate() {
		ui(move |u| u.show_error(&format!("config: {e}")));
		return;
	}
	core.apply_config();
	log::info!("Saving configuration!");
	if let Err(e) = core.cfg().save() {
		let msg = format!("save config: {e}");
		ui(move |u| u.show_error(&msg));
	}
}

impl Manager {
	pub fn new(u: &Rc<Ui>) -> Rc<Manager> {
		let b = gtk::Builder::from_resource(&resource("ui/manager.ui"));
		let win: adw::ApplicationWindow = obj(&b, "window");
		win.set_application(Some(&u.app));
		let m = Rc::new(Manager {
			navigation: obj(&b, "navigation"),
			page: obj(&b, "main-page"),
			wine_row: obj(&b, "wine_row"),
			overlay: obj(&b, "overlay"),
			tags: obj(&b, "wine_tags"),
			spinner: RefCell::new(None),
			win,
		});
		m.win.connect_close_request(|_| {
			ui(|u| {
				u.mgr.borrow_mut().take();
			});
			glib::Propagation::Proceed
		});
		for hb in ["headerbar", "welcome_headerbar"] {
			crate::icon::header_bar(&obj(&b, hb));
		}
		m.actions(u, &b);
		m.bind(u, &b);
		m.wine_updater(u, &b);
		m
	}

	pub fn toast(&self, s: &str) {
		log::info!("{s}");
		let t = adw::Toast::new(s);
		t.set_priority(adw::ToastPriority::High);
		self.overlay.add_toast(t);
	}

	fn actions(self: &Rc<Self>, u: &Rc<Ui>, b: &gtk::Builder) {
		let core = u.core.clone();
		let cmd: gtk::Entry = obj(b, "cmd");
		{
			let core = core.clone();
			cmd.connect_activate(move |e| {
				let args: Vec<String> = e.text().split_whitespace().map(str::to_owned).collect();
				if args.is_empty() {
					return;
				}
				let core = core.clone();
				Ui::spawn(move || {
					let r = core.prepare_wine().and_then(|_| Ok(core.pfx().wine(&args[0], &args[1..]).run()?));
					core.release_hold();
					r
				});
			});
		}

		let add = |name: &str, f: Box<dyn Fn()>| {
			let a = gio::SimpleAction::new(name, None);
			a.connect_activate(move |_, _| f());
			self.win.add_action(&a);
		};
		// Background actions: run on a thread, then toast on success.
		let bg = |name: &str, toast: Option<String>, f: fn(&Core) -> Result| {
			let core = core.clone();
			let toast = toast.clone();
			add(
				name,
				Box::new(move || {
					let core = core.clone();
					let toast = toast.clone();
					Ui::spawn(move || {
						f(&core)?;
						if let Some(t) = toast {
							ui(move |u| {
								if let Some(m) = &*u.mgr.borrow() {
									m.toast(&t);
								}
							});
						}
						Ok(())
					});
				}),
			);
		};

		{
			let core = core.clone();
			add("save", Box::new(move || save(&core)));
		}
		{
			let w = self.win.clone();
			let version = u.version.clone();
			let core = core.clone();
			add(
				"about",
				Box::new(move || {
					let d = adw::AboutDialog::from_appdata(&resource("metainfo.xml"), Some(&version));
					let sm = adw::StyleManager::default();
					d.set_application_icon(crate::icon::Variant::for_windows(&core.cfg().icon, &sm).icon_name());
					d.present(Some(&w));
				}),
			);
		}
		for (name, dir) in [("open-data", DIRS.data.clone()), ("open-logs", DIRS.logs.clone())] {
			let w = self.win.clone();
			add(
				name,
				Box::new(move || {
					let uri = glib::filename_to_uri(&dir, None).map(|u| u.to_string()).unwrap_or_default();
					gtk::UriLauncher::new(&uri).launch(Some(&w), gio::Cancellable::NONE, |_| {});
				}),
			);
		}
		bg("prefix-kill", Some(l("Stopped all processes")), Core::kill_prefix);
		bg("delete-prefix", Some(l("Deleted Wine data")), Core::delete_prefixes);
		bg("delete-studio", Some(l("Uninstalled studio")), Core::delete_deployments);
		bg("clear-cache", Some(l("Cleared cache")), Core::clear_cache);
		bg("update", None, |c| c.update_wine("Latest"));
		bg("restore", None, Core::restore_settings);
		{
			let cmd = cmd.clone();
			add(
				"winecfg",
				Box::new(move || {
					cmd.set_text("winecfg");
					cmd.emit_activate();
				}),
			);
		}
	}

	fn wine_updater(self: &Rc<Self>, u: &Rc<Ui>, b: &gtk::Builder) {
		let tags = self.tags.clone();
		let selected: gtk::DropDown = obj(b, "wine_tag");
		let popover: gtk::Popover = obj(b, "wine_updater");
		{
			let tags = tags.clone();
			let selected = selected.clone();
			popover.connect_show(move |_| {
				if tags.n_items() > 1 {
					return;
				}
				log::info!("Fetching releases");
				let current = std::fs::canonicalize(&DIRS.wine_path)
					.ok()
					.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
					.map(|n| n.trim_start_matches(TAG_PREFIX).to_owned());
				if let Some(t) = &current {
					tags.append(t);
					selected.set_selected(1);
				}
				Ui::spawn(move || {
					let releases = crate::netutil::kombucha_releases()?;
					let names: Vec<String> = releases
						.into_iter()
						.map(|r| r.tag_name)
						.filter(|t| Some(t) != current.as_ref())
						.collect();
					ui(move |u| {
						if let Some(m) = &*u.mgr.borrow() {
							m.append_tags(&names);
						}
					});
					Ok(())
				});
			});
		}
		let confirm: gtk::Button = obj(b, "wine_confirm");
		let row = self.wine_row.clone();
		let core = u.core.clone();
		confirm.connect_clicked(move |_| {
			let Some(tag) = selected
				.selected_item()
				.and_downcast::<gtk::StringObject>()
				.map(|s| s.string().to_string())
			else {
				return;
			};
			let spin = adw::Spinner::new();
			row.set_sensitive(false);
			row.add_suffix(&spin);
			super::local(move |u| {
				if let Some(m) = &*u.mgr.borrow() {
					m.spinner.replace(Some(spin));
				}
			});
			let core = core.clone();
			std::thread::spawn(move || {
				let r = core.update_wine(&tag);
				ui(move |u| {
					if let Some(m) = &*u.mgr.borrow() {
						m.wine_row.set_sensitive(true);
						if let Some(s) = m.spinner.take() {
							m.wine_row.remove(&s);
						}
					}
					if let Err(e) = r {
						u.show_error(&e.to_string());
					}
				});
			});
		});
	}

	fn append_tags(&self, names: &[String]) {
		for n in names {
			self.tags.append(n);
		}
	}

	/// Binds every setting to its widget, in the order they appear.
	fn bind(self: &Rc<Self>, u: &Rc<Ui>, b: &gtk::Builder) {
		let core = u.core.clone();
		let cfg = core.cfg();
		let st = cfg.studio.clone();

		let entry = |name: &str, value: &str, set: fn(&mut Config, String)| {
			let row: adw::EntryRow = obj(b, name);
			row.set_text(value);
			let core = core.clone();
			row.connect_apply(move |r| {
				set(&mut core.cfg.write().unwrap(), r.text().to_string());
				save(&core);
			});
		};
		let switch = |name: &str, value: bool, set: fn(&mut Config, bool)| {
			let row: adw::SwitchRow = obj(b, name);
			row.set_active(value);
			let core = core.clone();
			row.connect_active_notify(move |r| {
				set(&mut core.cfg.write().unwrap(), r.is_active());
				save(&core);
			});
		};

		switch("webpages_row", !st.webview.is_empty(), |c, on| {
			c.studio.webview = if on { config::WEBVIEW_VERSION.into() } else { String::new() };
		});

		self.wine_row.set_subtitle(&st.wineroot);
		{
			let core = core.clone();
			self.wine_row.connect_subtitle_notify(move |r| {
				core.cfg.write().unwrap().studio.wineroot = r.subtitle().map(|s| s.to_string()).unwrap_or_default();
				save(&core);
			});
		}
		{
			let row = self.wine_row.clone();
			// Back to the default: Salt's own Wine, or none where there
			// is no build for this system.
			obj::<gtk::Button>(b, "wine_revert").connect_clicked(move |_| {
				row.set_subtitle(&config::Studio::default().wineroot);
			});
		}
		{
			let row = self.wine_row.clone();
			obj::<gtk::Button>(b, "wine_select").connect_clicked(move |btn| {
				let d = gtk::FileDialog::new();
				let row = row.clone();
				let win = btn.root().and_downcast::<gtk::Window>();
				d.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| match r {
					Ok(f) => {
						if let Some(p) = f.path() {
							row.set_subtitle(&p.display().to_string());
						}
					}
					Err(e) => log::error!("FileDialog error: {e}"),
				});
			});
		}

		// The model must list renderers in config::RENDERERS order.
		let renderer: adw::ComboRow = obj(b, "renderer_row");
		renderer.set_selected(
			config::RENDERERS
				.iter()
				.position(|r| *r == st.renderer)
				.unwrap_or(0) as u32,
		);
		{
			let core = core.clone();
			renderer.connect_selected_item_notify(move |r| {
				if let Some(name) = config::RENDERERS.get(r.selected() as usize) {
					core.cfg.write().unwrap().studio.renderer = (*name).into();
					save(&core);
				}
			});
		}

		let desktop: adw::ExpanderRow = obj(b, "desktop_row");
		let resolution: adw::EntryRow = obj(b, "resolution_entry");
		desktop.set_enable_expansion(!st.desktop.is_empty());
		resolution.set_text(&st.desktop);
		{
			let core = core.clone();
			let desktop = desktop.clone();
			resolution.connect_text_notify(move |r| {
				let text = r.text().trim().to_string();
				if text.is_empty() {
					desktop.set_expanded(false);
					desktop.set_enable_expansion(false);
				} else if !config::valid_resolution(&text) {
					// Mid-typing ("1920x"): wait for a whole one.
					r.add_css_class("error");
					return;
				}
				r.remove_css_class("error");
				core.cfg.write().unwrap().studio.desktop = text;
				save(&core);
			});
		}
		{
			let resolution = resolution.clone();
			desktop.connect_enable_expansion_notify(move |d| {
				if !d.enables_expansion() && !d.is_expanded() {
					resolution.set_text("");
				} else if resolution.text().is_empty() {
					resolution.set_text(config::DESKTOP_RESOLUTION);
				}
			});
		}

		let card: adw::ComboRow = obj(b, "cards_row");
		let cards: gtk::StringList = obj(b, "cards");
		for (i, c) in INFO.cards.iter().enumerate() {
			cards.append(&format!("{}: {}", c.index, c.product));
			if c.addr() == st.forced_gpu {
				card.set_selected(i as u32 + 1);
			}
		}
		{
			let core = core.clone();
			card.connect_selected_item_notify(move |r| {
				core.cfg.write().unwrap().studio.forced_gpu = match r.selected() {
					0 => String::new(),
					i => INFO.cards.get(i as usize - 1).map(|c| c.addr()).unwrap_or_default(),
				};
				save(&core);
			});
		}

		entry("launcher_row", &st.launcher, |c, v| c.studio.launcher = v);

		let theme: adw::ComboRow = obj(b, "theme_row");
		theme.set_selected(config::THEMES.iter().position(|s| *s == cfg.theme).unwrap_or(0) as u32);
		{
			let core = core.clone();
			theme.connect_selected_item_notify(move |r| {
				if let Some(s) = config::THEMES.get(r.selected() as usize) {
					core.cfg.write().unwrap().theme = (*s).into();
					save(&core);
					super::apply_theme(s);
				}
			});
		}

		let icon: adw::ComboRow = obj(b, "icon_row");
		icon.set_selected(crate::icon::SETTINGS.iter().position(|s| *s == cfg.icon).unwrap_or(0) as u32);
		{
			let core = core.clone();
			icon.connect_selected_item_notify(move |r| {
				if let Some(s) = crate::icon::SETTINGS.get(r.selected() as usize) {
					core.cfg.write().unwrap().icon = (*s).into();
					save(&core);
					super::local(|u| {
						if let Some(f) = &*u.update_icon.borrow() {
							f();
						}
					});
				}
			});
		}
		switch("discord_row", st.discord_rpc, |c, v| c.studio.discord_rpc = v);
		switch("gamemode_row", st.gamemode, |c, v| c.studio.gamemode = v);
		switch("debug_row", cfg.debug, |c, v| c.debug = v);
		switch("auto_update_row", cfg.auto_update, |c, v| c.auto_update = v);

		self.bind_env(&core, b);
		self.bind_fflags(&core, b);

		entry("version_row", &st.forced_version, |c, v| c.studio.forced_version = v);
		entry("channel_row", &st.channel, |c, v| c.studio.channel = v);
	}

	fn bind_env(&self, core: &Arc<Core>, b: &gtk::Builder) {
		let env: adw::ExpanderRow = obj(b, "env_row");
		for (k, v) in core.cfg().studio.env {
			env_row(core, &env, &k, &v);
		}
		let popover: gtk::Popover = obj(b, "env_popover");
		let core = core.clone();
		obj::<gtk::Entry>(b, "env_entry").connect_activate(move |e| {
			let text = e.text().to_string();
			let (key, val) = text.split_once('=').unwrap_or((&text, ""));
			if key.is_empty() || core.cfg().studio.env.contains_key(key) {
				e.add_css_class("error");
				return;
			}
			e.remove_css_class("error");
			core.cfg.write().unwrap().studio.env.insert(key.into(), val.into());
			env_row(&core, &env, key, val);
			save(&core);
			popover.popdown();
		});
	}

	fn bind_fflags(&self, core: &Arc<Core>, b: &gtk::Builder) {
		let fflags: adw::ExpanderRow = obj(b, "fflags_row");
		for (k, v) in core.cfg().studio.fflags {
			fflag_row(core, &fflags, &k, &v);
		}
		let kind: gtk::DropDown = obj(b, "fflag_type");
		let popover: gtk::Popover = obj(b, "fflag_popover");
		let core = core.clone();
		obj::<gtk::Entry>(b, "fflag_name").connect_activate(move |e| {
			let key = e.text().to_string();
			if key.is_empty() || core.cfg().studio.fflags.contains_key(&key) {
				e.add_css_class("error");
				return;
			}
			e.remove_css_class("error");
			let kind = kind
				.selected_item()
				.and_downcast::<gtk::StringObject>()
				.map(|s| s.string().to_string())
				.unwrap_or_default();
			// A default value of the type, which the user then edits.
			let v = match kind.as_str() {
				"Boolean" => rbxbin::FFlag::Bool(false),
				"String" => rbxbin::FFlag::String(String::new()),
				_ => rbxbin::FFlag::Int(0),
			};
			core.cfg.write().unwrap().studio.fflags.insert(key.clone(), v.clone());
			fflag_row(&core, &fflags, &key, &v);
			save(&core);
			popover.popdown();
		});
	}
}

fn remove_button(on_remove: impl Fn() + 'static) -> gtk::Button {
	let b = gtk::Button::from_icon_name("edit-delete-symbolic");
	b.set_valign(gtk::Align::Center);
	b.add_css_class("flat");
	b.connect_clicked(move |_| on_remove());
	b
}

fn env_row(core: &Arc<Core>, parent: &adw::ExpanderRow, key: &str, val: &str) {
	let row = adw::EntryRow::new();
	row.set_title(key);
	row.set_text(val);
	row.add_css_class("monospace");
	{
		let core = core.clone();
		let key = key.to_owned();
		row.connect_text_notify(move |r| {
			core.cfg.write().unwrap().studio.env.insert(key.clone(), r.text().to_string());
			save(&core);
		});
	}
	let (core2, key2, parent2, row2) = (core.clone(), key.to_owned(), parent.clone(), row.clone());
	row.add_suffix(&remove_button(move || {
		core2.cfg.write().unwrap().studio.env.remove(&key2);
		save(&core2);
		parent2.remove(&row2);
	}));
	parent.set_expanded(true);
	parent.add_row(&row);
}

fn fflag_row(core: &Arc<Core>, parent: &adw::ExpanderRow, key: &str, val: &rbxbin::FFlag) {
	let set = {
		let core = core.clone();
		let key = key.to_owned();
		move |v: rbxbin::FFlag| {
			core.cfg.write().unwrap().studio.fflags.insert(key.clone(), v);
			save(&core);
		}
	};
	let row: adw::PreferencesRow = match val {
		rbxbin::FFlag::Bool(b) => {
			let r = adw::SwitchRow::new();
			r.set_active(*b);
			r.connect_active_notify(move |r| set(rbxbin::FFlag::Bool(r.is_active())));
			r.upcast()
		}
		rbxbin::FFlag::Int(i) => {
			let adj = gtk::Adjustment::new(0.0, i64::MIN as f64, i64::MAX as f64, 1.0, 4.0, 0.0);
			let r = adw::SpinRow::new(Some(&adj), 1.0, 0);
			r.set_value(*i as f64);
			r.connect_value_notify(move |r| set(rbxbin::FFlag::Int(r.value() as i64)));
			r.upcast()
		}
		rbxbin::FFlag::Float(f) => {
			let adj = gtk::Adjustment::new(0.0, f64::MIN, f64::MAX, 0.1, 1.0, 0.0);
			let r = adw::SpinRow::new(Some(&adj), 0.1, 3);
			r.set_value(*f);
			r.connect_value_notify(move |r| set(rbxbin::FFlag::Float(r.value())));
			r.upcast()
		}
		rbxbin::FFlag::String(s) => {
			let r = adw::EntryRow::new();
			r.set_text(s);
			r.add_css_class("monospace");
			r.connect_text_notify(move |r| set(rbxbin::FFlag::String(r.text().to_string())));
			r.upcast()
		}
	};
	row.set_title(key);
	let (core2, key2, parent2, row2) = (core.clone(), key.to_owned(), parent.clone(), row.clone());
	let btn = remove_button(move || {
		core2.cfg.write().unwrap().studio.fflags.remove(&key2);
		save(&core2);
		parent2.remove(&row2);
	});
	if let Some(r) = row.downcast_ref::<adw::ActionRow>() {
		r.add_suffix(&btn);
	} else if let Some(r) = row.downcast_ref::<adw::EntryRow>() {
		r.add_suffix(&btn);
	}
	parent.set_expanded(true);
	parent.add_row(&row);
}
