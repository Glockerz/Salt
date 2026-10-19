//! Salt: Roblox Studio on Linux through Wine.

mod config;
mod core;
mod dirs;
mod discord;
mod gamemode;
mod i18n;
mod logging;
mod mcp;
mod migrate;
mod netutil;
mod robloxlog;
mod studiorpc;
mod sysinfo;
mod ui;
mod winehost;

use std::process::ExitCode;

fn main() -> ExitCode {
	// glibc sends A and AAAA queries in parallel from one socket; some
	// routers drop one of them and every such lookup stalls for the full
	// 5 s resolver timeout. Sequential queries avoid it, for Salt and
	// for Wine, which resolves Studio's hosts through the same resolver.
	if std::env::var_os("RES_OPTIONS").is_none() {
		// SAFETY: still single-threaded.
		unsafe { std::env::set_var("RES_OPTIONS", "single-request-reopen") };
	}

	match std::env::args().nth(1).as_deref() {
		Some("mcp") => return mcp::run(),
		Some("fix-paths") => return migrate::run(&std::env::args().skip(2).collect::<Vec<_>>()),
		Some("--version" | "version") => {
			println!("salt {}", env!("CARGO_PKG_VERSION"));
			return ExitCode::SUCCESS;
		}
		_ => {}
	}

	if sysinfo::INFO.cards.iter().any(|c| c.driver.starts_with("nvidia"))
		&& std::env::var_os("WAYLAND_DISPLAY").is_some()
	{
		// GTK panics with "No provider of eglGetCurrentContext found."
		// SAFETY: still single-threaded.
		unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
	}
	// Avoids VK_SUBOPTIMAL_KHR in GTK's own renderer.
	unsafe { std::env::set_var("GDK_DISABLE", "vulkan") };

	handle_signals();
	i18n::init();
	logging::init();
	let code = ui::run();
	logging::flush();
	if code == gtk::glib::ExitCode::SUCCESS {
		ExitCode::SUCCESS
	} else {
		ExitCode::FAILURE
	}
}

/// SIGINT/SIGTERM kill the Studio process Salt launched, which then
/// lets the bootstrapper finish and the app quit normally. The signals
/// are blocked before any thread exists and taken by one `sigwait`
/// thread, so no other thread is interrupted by them.
fn handle_signals() {
	// SAFETY: sigset manipulation on a local set; called before any
	// other thread is spawned, so the mask is inherited by all of them.
	unsafe {
		let mut set: libc::sigset_t = std::mem::zeroed();
		libc::sigemptyset(&mut set);
		libc::sigaddset(&mut set, libc::SIGINT);
		libc::sigaddset(&mut set, libc::SIGTERM);
		libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
		let set = set;
		std::thread::spawn(move || {
			loop {
				let mut sig = 0;
				if libc::sigwait(&set, &mut sig) != 0 {
					return;
				}
				log::warn!("Received signal signal={sig}");
				ui::ui(|u| {
					if u.core.child.load(std::sync::atomic::Ordering::SeqCst) == 0 {
						gtk::prelude::ApplicationExt::quit(&u.app);
					} else {
						u.core.kill_child();
					}
				});
			}
		});
	}
}
