//! Loads the layer through the Vulkan loader and exercises instance and
//! device creation: `VK_ADD_LAYER_PATH=... cargo run --example smoke`
use ash::vk;

fn main() {
	unsafe {
		let entry = ash::Entry::load().expect("vulkan loader");
		let layers = entry.enumerate_instance_layer_properties().unwrap();
		let ours = layers.iter().any(|l| l.layer_name_as_c_str().unwrap() == c"VK_LAYER_SALT_SaltLayer");
		println!("layer visible: {ours}");
		let names = [c"VK_LAYER_SALT_SaltLayer".as_ptr()];
		let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
		let ci = vk::InstanceCreateInfo::default().application_info(&app).enabled_layer_names(&names);
		let instance = entry.create_instance(&ci, None).expect("create instance");
		let gpus = instance.enumerate_physical_devices().unwrap();
		for (i, pd) in gpus.iter().enumerate() {
			let props = instance.get_physical_device_properties(*pd);
			let exts = instance.enumerate_device_extension_properties(*pd).unwrap();
			println!("gpu {i}: {:?} ({} extensions)", props.device_name_as_c_str().unwrap(), exts.len());
			let prio = [1.0];
			let q = [vk::DeviceQueueCreateInfo::default().queue_family_index(0).queue_priorities(&prio)];
			let dev_exts = [ash::khr::swapchain::NAME.as_ptr()];
			let dci = vk::DeviceCreateInfo::default().queue_create_infos(&q).enabled_extension_names(&dev_exts);
			let device = instance.create_device(*pd, &dci, None).expect("create device");
			let f = instance.get_device_proc_addr(device.handle(), c"vkAcquireNextImageKHR".as_ptr());
			println!("  device ok, vkAcquireNextImageKHR hooked: {}", f.is_some());
			device.destroy_device(None);
		}
		instance.destroy_instance(None);
		println!("clean shutdown");
	}
}
