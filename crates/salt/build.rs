fn main() {
	glib_build_tools::compile_resources(
		&["../../data"],
		"../../data/salt.gresource.xml",
		"salt.gresource",
	);
	println!("cargo:rerun-if-changed=../../data");
}
