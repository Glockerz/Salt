use std::time::Instant;
fn main() {
	let c = rbxweb::Client::new();
	for _ in 0..2 {
		let t = Instant::now();
		let r = c.user_channel(Some(rbxweb::BinaryType::WindowsStudio64));
		println!("user_channel {:?} {:?}", t.elapsed(), r.map(|u| u.channel));
		let t = Instant::now();
		let r = c.client_version(rbxweb::BinaryType::WindowsStudio64, "LIVE");
		println!("client_version {:?} {:?}", t.elapsed(), r.map(|v| v.guid));
	}
	let t = Instant::now();
	let c2 = c.with_channel_token(None);
	println!("with_channel_token {:?}", t.elapsed());
	let t = Instant::now();
	let r = c2.client_version(rbxweb::BinaryType::WindowsStudio64, "LIVE");
	println!("new client client_version {:?} {:?}", t.elapsed(), r.is_ok());
	let t = Instant::now();
	let r = ureq::get("https://clientsettings.roblox.com/v2/user-channel").call().map(|mut r| r.body_mut().read_to_string());
	println!("plain ureq {:?} {:?}", t.elapsed(), r.is_ok());
}
