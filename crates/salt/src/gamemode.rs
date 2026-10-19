//! GameMode registration through the XDG desktop portal.

use std::os::fd::{FromRawFd, OwnedFd};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

fn pidfd_open(pid: u32) -> std::io::Result<OwnedFd> {
	// SAFETY: pidfd_open returns a new fd or -1.
	let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
	if fd < 0 {
		return Err(std::io::Error::last_os_error());
	}
	// SAFETY: the fd was just created and is owned by nobody else.
	Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

/// Registers a process with GameMode. The portal only says "rejected"
/// when the GameMode daemon isn't running.
pub fn register(pid: u32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
	let target = pidfd_open(pid)?;
	let requester = pidfd_open(std::process::id())?;

	let fds = gio::UnixFDList::new();
	fds.append(&target)?;
	fds.append(&requester)?;

	let params = glib::Variant::tuple_from_iter([
		glib::variant::Handle(0).to_variant(),
		glib::variant::Handle(1).to_variant(),
	]);
	let (resp, _) = bus.call_with_unix_fd_list_sync(
		Some("org.freedesktop.portal.Desktop"),
		"/org/freedesktop/portal/desktop",
		"org.freedesktop.portal.GameMode",
		"RegisterGameByPIDFd",
		Some(&params),
		Some(glib::VariantTy::new("(i)").unwrap()),
		gio::DBusCallFlags::NONE,
		-1,
		Some(&fds),
		gio::Cancellable::NONE,
	)?;
	let (res,): (i32,) = resp.get().ok_or("unexpected reply")?;
	if res < 0 {
		return Err("rejected".into());
	}
	log::info!("Registered with GameMode response={res}");
	Ok(())
}
