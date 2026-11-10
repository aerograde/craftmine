//! E3's core: the translucent-draw keys sorted by a bitonic network on a
//! compute pass, both ways. One workgroup holds up to [`CAPACITY`] keys and
//! runs the whole network with barriers; the renderer's draw path consumes
//! the sorted buffer. Larger lists sort in the CPU path meanwhile.
//!
//! The CPU reference runs the same network, so the order is testable without
//! a GPU; the shader is validated by naga and its pipeline and dispatch by
//! wgpu's no-op backend (see [`gpu_test`](super::gpu_test)).

/// The most keys one workgroup sorts, and the storage buffer's length.
pub const CAPACITY: usize = 1024;

/// The CPU reference for [`SORT_WGSL`], on all of `keys` and ascending.
pub fn sort_cpu(keys: &mut [u32; CAPACITY]) {
    let n = CAPACITY as u32;
    let mut k = 2u32;
    while k <= n {
        let mut j = k / 2;
        while j > 0 {
            let mut i = 0u32;
            while i < n {
                let l = i ^ j;
                if l > i {
                    let ascending = (i & k) == 0;
                    let (a, b) = (keys[i as usize], keys[l as usize]);
                    if (a > b) == ascending {
                        keys[i as usize] = b;
                        keys[l as usize] = a;
                    }
                }
                i += 1;
            }
            j /= 2;
        }
        k *= 2;
    }
}

/// The compute shader: 256 invocations load four keys each into a workgroup
/// array, run the bitonic network over [`CAPACITY`], and write it back
/// ascending.
pub const SORT_WGSL: &str = r#"
const N: u32 = 1024u;
var<workgroup> data: array<u32, 1024u>;
@group(0) @binding(0) var<storage, read_write> keys: array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) local: vec3<u32>) {
    let thread = local.x;
    for (var e = 0u; e < 4u; e = e + 1u) {
        let index = thread * 4u + e;
        data[index] = keys[index];
    }
    workgroupBarrier();
    for (var k = 2u; k <= N; k = k * 2u) {
        for (var j = k / 2u; j > 0u; j = j / 2u) {
            for (var e = 0u; e < 4u; e = e + 1u) {
                let index = thread * 4u + e;
                let partner = index ^ j;
                if (partner > index) {
                    let ascending = (index & k) == 0u;
                    let a = data[index];
                    let b = data[partner];
                    if ((a > b) == ascending) {
                        data[index] = b;
                        data[partner] = a;
                    }
                }
            }
            workgroupBarrier();
        }
    }
    for (var e = 0u; e < 4u; e = e + 1u) {
        let index = thread * 4u + e;
        keys[index] = data[index];
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_reference_sorts_like_a_sort() {
        let mut keys = [0u32; CAPACITY];
        // A deterministic shuffle: reverse plus a stride permutation.
        for (i, key) in keys.iter_mut().enumerate() {
            *key = (CAPACITY as u32 - 1 - i as u32).wrapping_mul(2654435761);
        }
        let mut expected = keys;
        expected.sort_unstable();
        sort_cpu(&mut keys);
        assert_eq!(keys, expected);
        // Equal and already sorted keys stay put.
        let mut equal = [7u32; CAPACITY];
        sort_cpu(&mut equal);
        assert!(equal.iter().all(|&k| k == 7));
    }

    #[test]
    fn the_sort_shader_validates() {
        let module = naga::front::wgsl::parse_str(SORT_WGSL)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SORT_WGSL)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(module.entry_points[0].name, "main");
        assert_eq!(module.entry_points[0].workgroup_size, [256, 1, 1]);
        let bindings: Vec<u32> = module
            .global_variables
            .iter()
            .filter_map(|(_, var)| var.binding.as_ref().map(|b| b.binding))
            .collect();
        assert_eq!(bindings, [0]);
    }

    /// With no graphics adapter, wgpu's no-op backend still validates the
    /// shader, the pipeline and the dispatch record.
    #[test]
    fn the_sort_pipeline_creates_and_dispatches_without_a_gpu() {
        let (device, queue) = super::super::gpu_test::noop_device();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bitonic sort"),
            source: wgpu::ShaderSource::Wgsl(SORT_WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("bitonic sort"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let keys = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("keys"),
            size: (CAPACITY * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bitonic sort"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: keys.as_entire_binding(),
            }],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);
    }
}
