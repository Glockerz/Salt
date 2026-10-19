fn main() {
	glib_build_tools::compile_resources(
		&["../../data"],
		"../../data/vinegar.gresource.xml",
		"vinegar.gresource",
	);
	println!("cargo:rerun-if-changed=../../data");
}
