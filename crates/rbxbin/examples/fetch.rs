//! Resolves the current Studio deployment, optionally installing it:
//! `cargo run --release --example fetch -- [GUID] [DEST_DIR]`
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let args: Vec<String> = std::env::args().collect();
	let c = rbxweb::Client::new();
	let t = Instant::now();
	let d = match args.get(1).filter(|g| !g.is_empty()) {
		Some(g) => rbxbin::Deployment { kind: rbxbin::BinaryType::WindowsStudio64, channel: String::new(), guid: g.clone(), client: None },
		None => rbxbin::get_deployment(&c, rbxbin::BinaryType::WindowsStudio64, "")?,
	};
	println!("deployment {} channel={:?} ({:?})", d.guid, d.channel, t.elapsed());
	let t = Instant::now();
	let m = rbxbin::Mirror::get()?;
	println!("mirror {} ({:?})", m.0, t.elapsed());
	let t = Instant::now();
	let (pkgs, dirs) = std::thread::scope(|s| {
		let p = s.spawn(|| m.packages(&d));
		let dirs = m.binary_directories(&d);
		(p.join().unwrap(), dirs)
	});
	let (pkgs, dirs) = (pkgs?, dirs?);
	println!("{} packages, {} dirs ({:?})", pkgs.len(), dirs.len(), t.elapsed());
	let total: u64 = pkgs.iter().map(|p| p.zip_size).sum();
	println!("download size {:.1} MiB", total as f64 / 1048576.0);
	if let Some(dest) = args.get(2) {
		let dest = std::path::Path::new(dest);
		let t = Instant::now();
		let last = std::sync::Mutex::new(0u32);
		rbxbin::install(&d, &m, pkgs, &dirs, &dest.join("downloads"), &dest.join(&d.guid), 8,
			&|f| { let p = (f * 100.0) as u32; let mut l = last.lock().unwrap(); if p >= *l + 10 { *l = p; eprintln!("{p}%"); } },
			&AtomicBool::new(false))?;
		println!("installed in {:?}", t.elapsed());
	}
	Ok(())
}
