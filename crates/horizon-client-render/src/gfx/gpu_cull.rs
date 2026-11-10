//! E1's core: the frustum and cave-graph culling of the world's sections on a
//! compute pass, both ways. The CPU reference decides the same sections as
//! the shader; the shader is validated by naga, and its pipeline, bind groups
//! and dispatch by wgpu's no-op backend, so a machine without a graphics
//! adapter still covers the algorithm (see [`gpu_test`](super::gpu_test)).
//!
//! The pass appends the surviving section indices to a storage buffer; the
//! renderer's draw path reads them (one indirect draw over the compaction).
use glam::{Vec3, Vec4};

/// One section as the pass reads it: its camera-relative box (the mesh's
/// tight bounds) and whether the cave graph reaches it from the eye.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CullSection {
    pub min: Vec3,
    pub max: Vec3,
    pub reachable: bool,
    /// The arena page whose indirect draw this section belongs to.
    pub page: u32,
}

/// The CPU reference for [`CULL_WGSL`]: the indices of the reachable sections
/// whose box is not wholly behind any plane, in input order.
pub fn cull_cpu(sections: &[CullSection], planes: &[Vec4; 6]) -> Vec<u32> {
    sections
        .iter()
        .enumerate()
        .filter(|(_, section)| section.reachable)
        .filter(|(_, section)| {
            !planes
                .iter()
                .any(|plane| outside(*plane, section.min, section.max))
        })
        .map(|(index, _)| index as u32)
        .collect()
}

/// The per-page counts the shader's atomics produce from the same survivors:
/// the instance count each arena page's indirect draw takes.
pub fn page_counts_cpu(sections: &[CullSection], planes: &[Vec4; 6], pages: usize) -> Vec<u32> {
    let mut counts = vec![0; pages];
    for section in sections {
        if !section.reachable || section.page as usize >= pages {
            continue;
        }
        if planes
            .iter()
            .any(|plane| outside(*plane, section.min, section.max))
        {
            continue;
        }
        counts[section.page as usize] += 1;
    }
    counts
}

/// Whether the positive vertex of the box (the corner farthest along the
/// plane's normal) is behind the plane: then the whole box is.
fn outside(plane: Vec4, min: Vec3, max: Vec3) -> bool {
    let positive = Vec3::new(
        if plane.x >= 0.0 { max.x } else { min.x },
        if plane.y >= 0.0 { max.y } else { min.y },
        if plane.z >= 0.0 { max.z } else { min.z },
    );
    plane.truncate().dot(positive) + plane.w < 0.0
}

/// The compute shader: one invocation per section, appending survivors and
/// counting them per arena page for the indirect draws.
///
/// The section layout matches the renderer's use: `min` (12 bytes), a `u32`
/// reachable flag, `max` (12 bytes), then the page (28..32), 32 in all.
pub const CULL_WGSL: &str = r#"
struct Section {
    min: vec3<f32>,
    reachable: u32,
    max: vec3<f32>,
    page: u32,
}
struct Params {
    // count, pages
    count: vec4<u32>,
    pad: vec4<u32>,
}
@group(0) @binding(0) var<storage, read> sections: array<Section>;
@group(0) @binding(1) var<storage, read> planes: array<vec4<f32>, 6>;
@group(0) @binding(2) var<storage, read_write> visible: array<u32>;
@group(0) @binding(3) var<storage, read_write> count: atomic<u32>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read_write> page_counts: array<atomic<u32>>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index >= params.count.x) {
        return;
    }
    let section = sections[index];
    if (section.reachable == 0u || section.page >= params.count.y) {
        return;
    }
    for (var i = 0u; i < 6u; i = i + 1u) {
        let plane = planes[i];
        let positive = vec3<f32>(
            select(section.min.x, section.max.x, plane.x >= 0.0),
            select(section.min.y, section.max.y, plane.y >= 0.0),
            select(section.min.z, section.max.z, plane.z >= 0.0),
        );
        if (dot(plane.xyz, positive) + plane.w < 0.0) {
            return;
        }
    }
    let slot = atomicAdd(&count, 1u);
    visible[slot] = index;
    atomicAdd(&page_counts[section.page], 1u);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Six inward-facing planes of the box -10..10 on every axis.
    fn box_frustum() -> [Vec4; 6] {
        [
            Vec4::new(1.0, 0.0, 0.0, 10.0),
            Vec4::new(-1.0, 0.0, 0.0, 10.0),
            Vec4::new(0.0, 1.0, 0.0, 10.0),
            Vec4::new(0.0, -1.0, 0.0, 10.0),
            Vec4::new(0.0, 0.0, 1.0, 10.0),
            Vec4::new(0.0, 0.0, -1.0, 10.0),
        ]
    }

    fn section(min: Vec3, max: Vec3, reachable: bool) -> CullSection {
        CullSection {
            min,
            max,
            reachable,
            page: 0,
        }
    }

    fn page_section(min: Vec3, max: Vec3, reachable: bool, page: u32) -> CullSection {
        CullSection {
            min,
            max,
            reachable,
            page,
        }
    }

    #[test]
    fn the_cpu_reference_culls_like_the_frustum() {
        let sections = [
            // Inside.
            section(Vec3::splat(-1.0), Vec3::splat(1.0), true),
            // Past +x.
            section(Vec3::splat(11.0), Vec3::splat(13.0), true),
            // Past -z.
            section(Vec3::new(0.0, 0.0, -13.0), Vec3::new(2.0, 2.0, -11.0), true),
            // Inside, but the cave graph does not reach it.
            section(Vec3::splat(-2.0), Vec3::splat(-1.0), false),
            // Straddling the +y plane: its positive vertex is inside.
            section(Vec3::new(0.0, 9.0, 0.0), Vec3::new(2.0, 11.0, 2.0), true),
        ];
        assert_eq!(cull_cpu(&sections, &box_frustum()), vec![0, 4]);
    }

    #[test]
    fn the_per_page_counts_match_the_visible_sections() {
        let sections = [
            page_section(Vec3::splat(-1.0), Vec3::splat(1.0), true, 0),
            page_section(Vec3::splat(11.0), Vec3::splat(13.0), true, 1),
            page_section(Vec3::splat(-2.0), Vec3::splat(-1.0), true, 0),
            page_section(Vec3::splat(-3.0), Vec3::splat(-2.0), false, 1),
            page_section(Vec3::splat(0.0), Vec3::splat(1.0), true, 7),
        ];
        assert_eq!(
            page_counts_cpu(&sections, &box_frustum(), 4),
            vec![2, 0, 0, 0]
        );
    }

    /// The Rust and WGSL layouts are both checked, not assumed: the shader's
    /// struct offsets come from naga's parsed module and the Rust one's from
    /// `offset_of!`.
    #[test]
    fn the_section_layout_matches_the_shader() {
        #[repr(C)]
        struct GpuSection {
            min: [f32; 3],
            reachable: u32,
            max: [f32; 3],
            page: u32,
        }
        assert_eq!(std::mem::size_of::<GpuSection>(), 32);
        assert_eq!(std::mem::offset_of!(GpuSection, min), 0);
        assert_eq!(std::mem::offset_of!(GpuSection, reachable), 12);
        assert_eq!(std::mem::offset_of!(GpuSection, max), 16);
        assert_eq!(std::mem::offset_of!(GpuSection, page), 28);

        let module = naga::front::wgsl::parse_str(CULL_WGSL).unwrap();
        let (_, section) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Section"))
            .expect("Section");
        let naga::TypeInner::Struct { members, span } = &section.inner else {
            panic!("Section is not a struct");
        };
        assert_eq!(
            members.iter().map(|m| m.offset).collect::<Vec<_>>(),
            [0, 12, 16, 28]
        );
        assert_eq!(*span, 32);
        let (_, params) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Params"))
            .expect("Params");
        let naga::TypeInner::Struct { members, span } = &params.inner else {
            panic!("Params is not a struct");
        };
        assert_eq!(
            members.iter().map(|m| m.offset).collect::<Vec<_>>(),
            [0, 16]
        );
        assert_eq!(*span, 32);
    }

    #[test]
    fn the_cull_shader_validates() {
        let module = naga::front::wgsl::parse_str(CULL_WGSL)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(CULL_WGSL)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(module.entry_points.len(), 1);
        assert_eq!(module.entry_points[0].name, "main");
        assert_eq!(module.entry_points[0].workgroup_size, [64, 1, 1]);
        let bindings: Vec<u32> = module
            .global_variables
            .iter()
            .filter_map(|(_, var)| var.binding.as_ref().map(|b| b.binding))
            .collect();
        assert_eq!(bindings, [0, 1, 2, 3, 4, 5]);
    }

    /// With no graphics adapter, wgpu's no-op backend still validates the
    /// shader, the pipeline layout, the bind groups and the dispatch record.
    #[test]
    fn the_cull_pipeline_creates_and_dispatches_without_a_gpu() {
        let (device, queue) = super::super::gpu_test::noop_device();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cull"),
            source: wgpu::ShaderSource::Wgsl(CULL_WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cull"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let count = 128u64;
        let sections = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sections"),
            size: count * 32,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let planes = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("planes"),
            size: 6 * 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let visible = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("visible"),
            size: count * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let counter = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("count"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let page_counts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("page counts"),
            size: 4 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut params_bytes = [0u8; 32];
        params_bytes[0..4].copy_from_slice(&(count as u32).to_le_bytes());
        params_bytes[4..8].copy_from_slice(&4u32.to_le_bytes());
        queue.write_buffer(&params, 0, &params_bytes);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cull"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: sections.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: planes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: visible.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: counter.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: page_counts.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(count.div_ceil(64) as u32, 1, 1);
        }
        queue.submit([encoder.finish()]);
    }
}
