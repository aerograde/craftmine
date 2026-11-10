//! A graphics device with no GPU behind it: wgpu's no-op backend, for tests
//! that must check a pipeline, its bind groups and a pass record without an
//! adapter. Shader behaviour itself is checked by the CPU references beside
//! each shader and by naga's validation.
use std::sync::OnceLock;

/// A device and queue from wgpu's no-op backend, made once.
pub(crate) fn noop_device() -> &'static (wgpu::Device, wgpu::Queue) {
    static DEVICE: OnceLock<(wgpu::Device, wgpu::Queue)> = OnceLock::new();
    DEVICE.get_or_init(|| wgpu::Device::noop(&wgpu::DeviceDescriptor::default()))
}
