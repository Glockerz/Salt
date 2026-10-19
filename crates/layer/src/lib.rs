//! Vulkan layer to fix VK_ERROR_OUT_OF_DATE_KHR errors in Roblox Studio.
//!
//! Port of layer/vinegar_layer.cpp (by tunis, workaround by V3L0C1T13S),
//! itself based on https://github.com/baldurk/sample_layer/
//! (BSD 2-Clause, Copyright (c) 2016, Baldur Karlsson).
//!
//! Differences from the C++ version:
//! - Dispatch tables sit behind a read-write lock instead of one global
//!   mutex, and the per-frame vkAcquireNextImageKHR path only reads.
//! - The swapchain flag is an atomic.
//! - vkCreateInstance/vkCreateDevice failures are returned instead of
//!   being ignored and followed by use of an invalid handle.

#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, RwLock};

use ash::vk;

const LAYER_NAME: &CStr = c"VK_LAYER_VINEGAR_VinegarLayer";

// Loader-layer interface types (vk_layer.h), not part of ash.
const VK_LAYER_LINK_INFO: i32 = 0;

#[repr(C)]
struct LayerInstanceLink {
	next: *mut LayerInstanceLink,
	next_gipa: vk::PFN_vkGetInstanceProcAddr,
	next_gpdpa: *const c_void,
}

#[repr(C)]
struct LayerInstanceCreateInfo {
	s_type: vk::StructureType,
	p_next: *const c_void,
	function: i32,
	layer_info: *mut LayerInstanceLink,
}

#[repr(C)]
struct LayerDeviceLink {
	next: *mut LayerDeviceLink,
	next_gipa: vk::PFN_vkGetInstanceProcAddr,
	next_gdpa: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
struct LayerDeviceCreateInfo {
	s_type: vk::StructureType,
	p_next: *const c_void,
	function: i32,
	layer_info: *mut LayerDeviceLink,
}

#[derive(Clone, Copy)]
struct InstanceTable {
	gipa: vk::PFN_vkGetInstanceProcAddr,
	enumerate_device_extension_properties: Option<vk::PFN_vkEnumerateDeviceExtensionProperties>,
	surface_capabilities: Option<vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR>,
}

#[derive(Clone, Copy)]
struct DeviceTable {
	gdpa: vk::PFN_vkGetDeviceProcAddr,
	acquire_next_image: Option<vk::PFN_vkAcquireNextImageKHR>,
}

/// Dispatchable handles start with the loader's dispatch table pointer,
/// which identifies the instance or device across its children.
type Key = usize;

unsafe fn key<T>(handle: *const T) -> Key {
	unsafe { *(handle as *const usize) }
}

static INSTANCES: LazyLock<RwLock<HashMap<Key, InstanceTable>>> = LazyLock::new(Default::default);
static DEVICES: LazyLock<RwLock<HashMap<Key, DeviceTable>>> = LazyLock::new(Default::default);
static RECREATE_SWAPCHAIN: AtomicBool = AtomicBool::new(false);

unsafe fn load<T>(f: vk::PFN_vkVoidFunction) -> Option<T> {
	// SAFETY: the caller names the correct function pointer type.
	f.map(|f| unsafe { std::mem::transmute_copy::<_, T>(&f) })
}

unsafe extern "system" fn CreateInstance(
	info: *const vk::InstanceCreateInfo<'_>,
	alloc: *const vk::AllocationCallbacks<'_>,
	instance: *mut vk::Instance,
) -> vk::Result {
	unsafe {
		let mut ci = (*info).p_next as *mut LayerInstanceCreateInfo;
		while !ci.is_null()
			&& !((*ci).s_type == vk::StructureType::LOADER_INSTANCE_CREATE_INFO && (*ci).function == VK_LAYER_LINK_INFO)
		{
			ci = (*ci).p_next as *mut LayerInstanceCreateInfo;
		}
		if ci.is_null() {
			return vk::Result::ERROR_INITIALIZATION_FAILED;
		}
		let gipa = (*(*ci).layer_info).next_gipa;
		// Move the chain on for the next layer.
		(*ci).layer_info = (*(*ci).layer_info).next;

		let Some(create): Option<vk::PFN_vkCreateInstance> =
			load(gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()))
		else {
			return vk::Result::ERROR_INITIALIZATION_FAILED;
		};
		let ret = create(info, alloc, instance);
		if ret != vk::Result::SUCCESS {
			return ret;
		}
		let i = *instance;
		let table = InstanceTable {
			gipa: load(gipa(i, c"vkGetInstanceProcAddr".as_ptr())).unwrap_or(gipa),
			enumerate_device_extension_properties: load(gipa(i, c"vkEnumerateDeviceExtensionProperties".as_ptr())),
			surface_capabilities: load(gipa(i, c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR".as_ptr())),
		};
		INSTANCES.write().unwrap().insert(key(vk::Handle::as_raw(i) as *const c_void), table);
		vk::Result::SUCCESS
	}
}

unsafe extern "system" fn DestroyInstance(instance: vk::Instance, alloc: *const vk::AllocationCallbacks<'_>) {
	unsafe {
		let k = key(vk::Handle::as_raw(instance) as *const c_void);
		let table = INSTANCES.write().unwrap().remove(&k);
		if let Some(t) = table
			&& let Some(destroy) = load::<vk::PFN_vkDestroyInstance>((t.gipa)(instance, c"vkDestroyInstance".as_ptr()))
		{
			destroy(instance, alloc);
		}
	}
}

unsafe extern "system" fn CreateDevice(
	physical: vk::PhysicalDevice,
	info: *const vk::DeviceCreateInfo<'_>,
	alloc: *const vk::AllocationCallbacks<'_>,
	device: *mut vk::Device,
) -> vk::Result {
	unsafe {
		let mut ci = (*info).p_next as *mut LayerDeviceCreateInfo;
		while !ci.is_null()
			&& !((*ci).s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO && (*ci).function == VK_LAYER_LINK_INFO)
		{
			ci = (*ci).p_next as *mut LayerDeviceCreateInfo;
		}
		if ci.is_null() {
			return vk::Result::ERROR_INITIALIZATION_FAILED;
		}
		let gipa = (*(*ci).layer_info).next_gipa;
		let gdpa = (*(*ci).layer_info).next_gdpa;
		(*ci).layer_info = (*(*ci).layer_info).next;

		let Some(create): Option<vk::PFN_vkCreateDevice> =
			load(gipa(vk::Instance::null(), c"vkCreateDevice".as_ptr()))
		else {
			return vk::Result::ERROR_INITIALIZATION_FAILED;
		};
		let ret = create(physical, info, alloc, device);
		if ret != vk::Result::SUCCESS {
			return ret;
		}
		let d = *device;
		let table = DeviceTable {
			gdpa: load(gdpa(d, c"vkGetDeviceProcAddr".as_ptr())).unwrap_or(gdpa),
			acquire_next_image: load(gdpa(d, c"vkAcquireNextImageKHR".as_ptr())),
		};
		DEVICES.write().unwrap().insert(key(vk::Handle::as_raw(d) as *const c_void), table);
		vk::Result::SUCCESS
	}
}

unsafe extern "system" fn DestroyDevice(device: vk::Device, alloc: *const vk::AllocationCallbacks<'_>) {
	unsafe {
		let k = key(vk::Handle::as_raw(device) as *const c_void);
		let table = DEVICES.write().unwrap().remove(&k);
		if let Some(t) = table
			&& let Some(destroy) = load::<vk::PFN_vkDestroyDevice>((t.gdpa)(device, c"vkDestroyDevice".as_ptr()))
		{
			destroy(device, alloc);
		}
	}
}

unsafe extern "system" fn GetPhysicalDeviceSurfaceCapabilitiesKHR(
	physical: vk::PhysicalDevice,
	surface: vk::SurfaceKHR,
	caps: *mut vk::SurfaceCapabilitiesKHR,
) -> vk::Result {
	unsafe {
		let f = INSTANCES
			.read()
			.unwrap()
			.get(&key(vk::Handle::as_raw(physical) as *const c_void))
			.and_then(|t| t.surface_capabilities);
		let Some(f) = f else {
			return vk::Result::ERROR_INITIALIZATION_FAILED;
		};
		let result = f(physical, surface, caps);
		// Roblox recreates its swapchain when the surface is reported
		// lost, which recovers from out-of-date swapchains.
		if RECREATE_SWAPCHAIN.swap(false, Ordering::AcqRel) {
			return vk::Result::ERROR_SURFACE_LOST_KHR;
		}
		result
	}
}

unsafe extern "system" fn AcquireNextImageKHR(
	device: vk::Device,
	swapchain: vk::SwapchainKHR,
	timeout: u64,
	semaphore: vk::Semaphore,
	fence: vk::Fence,
	index: *mut u32,
) -> vk::Result {
	unsafe {
		let f = DEVICES
			.read()
			.unwrap()
			.get(&key(vk::Handle::as_raw(device) as *const c_void))
			.and_then(|t| t.acquire_next_image);
		let Some(f) = f else {
			return vk::Result::ERROR_DEVICE_LOST;
		};
		match f(device, swapchain, timeout, semaphore, fence, index) {
			// Suboptimal swapchains still work, but Roblox treats it as
			// fatal.
			vk::Result::SUBOPTIMAL_KHR => vk::Result::SUCCESS,
			vk::Result::ERROR_OUT_OF_DATE_KHR => {
				RECREATE_SWAPCHAIN.store(true, Ordering::Release);
				vk::Result::ERROR_OUT_OF_DATE_KHR
			}
			r => r,
		}
	}
}

unsafe fn copy_name(dst: &mut [c_char], src: &CStr) {
	for (d, s) in dst.iter_mut().zip(src.to_bytes_with_nul()) {
		*d = *s as c_char;
	}
}

unsafe extern "system" fn EnumerateInstanceLayerProperties(count: *mut u32, props: *mut vk::LayerProperties) -> vk::Result {
	unsafe {
		if !count.is_null() {
			*count = 1;
		}
		if !props.is_null() {
			let p = &mut *props;
			copy_name(&mut p.layer_name, LAYER_NAME);
			copy_name(&mut p.description, c"Vinegar layer");
			p.implementation_version = 1;
			p.spec_version = vk::API_VERSION_1_0;
		}
		vk::Result::SUCCESS
	}
}

unsafe extern "system" fn EnumerateDeviceLayerProperties(
	_: vk::PhysicalDevice,
	count: *mut u32,
	props: *mut vk::LayerProperties,
) -> vk::Result {
	unsafe { EnumerateInstanceLayerProperties(count, props) }
}

unsafe fn is_us(name: *const c_char) -> bool {
	!name.is_null() && unsafe { CStr::from_ptr(name) } == LAYER_NAME
}

unsafe extern "system" fn EnumerateInstanceExtensionProperties(
	name: *const c_char,
	count: *mut u32,
	_: *mut vk::ExtensionProperties,
) -> vk::Result {
	unsafe {
		if !is_us(name) {
			return vk::Result::ERROR_LAYER_NOT_PRESENT;
		}
		if !count.is_null() {
			*count = 0;
		}
		vk::Result::SUCCESS
	}
}

unsafe extern "system" fn EnumerateDeviceExtensionProperties(
	physical: vk::PhysicalDevice,
	name: *const c_char,
	count: *mut u32,
	props: *mut vk::ExtensionProperties,
) -> vk::Result {
	unsafe {
		if !is_us(name) {
			if physical == vk::PhysicalDevice::null() {
				return vk::Result::SUCCESS;
			}
			let f = INSTANCES
				.read()
				.unwrap()
				.get(&key(vk::Handle::as_raw(physical) as *const c_void))
				.and_then(|t| t.enumerate_device_extension_properties);
			return match f {
				Some(f) => f(physical, name, count, props),
				None => vk::Result::ERROR_INITIALIZATION_FAILED,
			};
		}
		if !count.is_null() {
			*count = 0;
		}
		vk::Result::SUCCESS
	}
}

macro_rules! intercept {
	($name:expr, $($f:ident),*) => {
		$(if $name == concat!("vk", stringify!($f)).as_bytes() {
			return Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>($f as *const ()));
		})*
	};
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn VinegarLayer_GetDeviceProcAddr(device: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
	unsafe {
		let n = CStr::from_ptr(name).to_bytes();
		intercept!(n, EnumerateDeviceLayerProperties, EnumerateDeviceExtensionProperties, CreateDevice, DestroyDevice, AcquireNextImageKHR);
		if n == b"vkGetDeviceProcAddr" {
			return Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>(
				VinegarLayer_GetDeviceProcAddr as *const (),
			));
		}
		let gdpa = DEVICES.read().unwrap().get(&key(vk::Handle::as_raw(device) as *const c_void)).map(|t| t.gdpa);
		gdpa.and_then(|f| f(device, name))
	}
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn VinegarLayer_GetInstanceProcAddr(
	instance: vk::Instance,
	name: *const c_char,
) -> vk::PFN_vkVoidFunction {
	unsafe {
		let n = CStr::from_ptr(name).to_bytes();
		intercept!(
			n,
			EnumerateInstanceLayerProperties,
			EnumerateInstanceExtensionProperties,
			CreateInstance,
			DestroyInstance,
			GetPhysicalDeviceSurfaceCapabilitiesKHR,
			EnumerateDeviceLayerProperties,
			EnumerateDeviceExtensionProperties,
			CreateDevice,
			DestroyDevice,
			AcquireNextImageKHR
		);
		if n == b"vkGetInstanceProcAddr" {
			return Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>(
				VinegarLayer_GetInstanceProcAddr as *const (),
			));
		}
		if n == b"vkGetDeviceProcAddr" {
			return Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>(
				VinegarLayer_GetDeviceProcAddr as *const (),
			));
		}
		if instance == vk::Instance::null() {
			return None;
		}
		let gipa = INSTANCES.read().unwrap().get(&key(vk::Handle::as_raw(instance) as *const c_void)).map(|t| t.gipa);
		gipa.and_then(|f| f(instance, name))
	}
}
