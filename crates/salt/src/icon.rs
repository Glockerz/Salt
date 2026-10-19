//! Salt's icon comes in a black and a white version. Inside Salt the
//! version follows the theme; for the application menu and taskbar,
//! which show one installed icon, Salt places the white version in the
//! user's icon directory (which takes priority over the installed black
//! one) while it is selected, and removes it otherwise.

use std::path::PathBuf;

use adw::prelude::*;

pub const SETTINGS: &[&str] = &["auto", "black", "white"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
	Black,
	White,
}

impl Variant {
	/// Icon name in Salt's resource icon theme.
	pub fn icon_name(self) -> &'static str {
		match self {
			Variant::Black => "salt-black",
			Variant::White => "salt-white",
		}
	}
}

const WHITE: &[(u32, &[u8])] = &[
	(32, include_bytes!("../../../data/icons/white/32x32/salt.png")),
	(48, include_bytes!("../../../data/icons/white/48x48/salt.png")),
	(64, include_bytes!("../../../data/icons/white/64x64/salt.png")),
	(128, include_bytes!("../../../data/icons/white/128x128/salt.png")),
	(256, include_bytes!("../../../data/icons/white/256x256/salt.png")),
	(512, include_bytes!("../../../data/icons/white/512x512/salt.png")),
];

/// Reports whether the desktop looks dark: an explicit dark preference,
/// or a GTK theme named as dark when there is no preference.
pub fn system_dark(sm: &adw::StyleManager) -> bool {
	if sm.is_dark() {
		return true;
	}
	if sm.system_supports_color_schemes() && sm.color_scheme() != adw::ColorScheme::Default {
		return false;
	}
	gtk::Settings::default()
		.and_then(|s| s.gtk_theme_name())
		.is_some_and(|n| n.to_lowercase().contains("dark"))
}

pub fn choose(setting: &str, dark: bool) -> Variant {
	match setting {
		"black" => Variant::Black,
		"white" => Variant::White,
		_ if dark => Variant::White,
		_ => Variant::Black,
	}
}

/// The host's user icon directory. Inside Flatpak, XDG_DATA_HOME points
/// into the sandbox, so this is derived from HOME.
fn user_icons() -> PathBuf {
	crate::dirs::home().join(".local/share/icons/hicolor")
}

/// Shows the variant in Salt's windows and the desktop's menus.
pub fn apply(v: Variant) {
	gtk::Window::set_default_icon_name(v.icon_name());
	crate::ui::local(|u| u.boot.logo.set_icon_name(Some(v.icon_name())));
	if let Err(e) = desktop(v) {
		log::debug!("Desktop icon not updated: {e}");
	}
}

fn desktop(v: Variant) -> std::io::Result<()> {
	let dir = user_icons();
	let name = format!("{}.png", crate::ui::APP_ID);
	let mut changed = false;
	for (size, png) in WHITE {
		let apps = dir.join(format!("{size}x{size}/apps"));
		let path = apps.join(&name);
		match v {
			Variant::White => {
				if std::fs::read(&path).is_ok_and(|cur| cur == *png) {
					continue;
				}
				std::fs::create_dir_all(&apps)?;
				std::fs::write(&path, png)?;
				changed = true;
			}
			Variant::Black => {
				if std::fs::remove_file(&path).is_ok() {
					changed = true;
				}
			}
		}
	}
	if changed {
		// Icon caches notice changes by the theme directory's mtime.
		let now = std::fs::FileTimes::new().set_modified(std::time::SystemTime::now());
		let _ = std::fs::File::open(&dir).and_then(|f| f.set_times(now));
		log::info!("Desktop icon set to {v:?}");
	}
	Ok(())
}

/// Applies the configured variant now and whenever the theme changes.
pub fn follow(app: &adw::Application, setting: impl Fn() -> String + 'static) -> impl Fn() + 'static {
	let sm = app.style_manager();
	let update = {
		let sm = sm.clone();
		move || apply(choose(&setting(), system_dark(&sm)))
	};
	update();
	let update = std::rc::Rc::new(update);
	{
		let u = update.clone();
		sm.connect_dark_notify(move |_| u());
	}
	if let Some(s) = gtk::Settings::default() {
		let u = update.clone();
		s.connect_gtk_theme_name_notify(move |_| u());
	}
	move || update()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn choice() {
		assert_eq!(choose("auto", true), Variant::White);
		assert_eq!(choose("auto", false), Variant::Black);
		assert_eq!(choose("white", false), Variant::White);
		assert_eq!(choose("black", true), Variant::Black);
	}
}
