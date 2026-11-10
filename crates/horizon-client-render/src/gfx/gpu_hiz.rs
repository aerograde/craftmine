//! E2's core: a screen box tested against a mip of a max-depth pyramid.
//!
//! Each texel holds the farthest depth in its region; a box is occluded when
//! the farthest depth it covers is nearer than the box's own nearest depth,
//! so every covered pixel has geometry in front of it. Missing texels of an
//! odd-sized level count as far, which keeps the test conservative.
//!
//! The CPU reference and [`HIZ_WGSL`] decide the same boxes, so the rule is
//! testable without a GPU; naga validates the shader and wgpu's no-op device
//! validates its pipeline and dispatch (see [`gpu_test`](super::gpu_test)).
use glam::Vec2;

/// A box projected to the mip's pixels, with its nearest camera depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiZBox {
    pub min: Vec2,
    pub max: Vec2,
    pub depth: f32,
}

/// One mip: `depth` is row-major, `width * height` floats in the camera's
/// depth convention (smaller is nearer).
#[derive(Clone, Debug, PartialEq)]
pub struct HiZLevel {
    pub width: u32,
    pub height: u32,
    pub depth: Vec<f32>,
}

impl HiZLevel {
    /// The max-depth mip below this one; an odd edge repeats its last texel.
    pub fn build(&self) -> HiZLevel {
        let width = self.width.div_ceil(2).max(1);
        let height = self.height.div_ceil(2).max(1);
        let mut depth = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let mut farthest = 0.0f32;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let sx = (x * 2 + dx).min(self.width - 1);
                        let sy = (y * 2 + dy).min(self.height - 1);
                        farthest = farthest.max(self.depth[(sy * self.width + sx) as usize]);
                    }
                }
                depth.push(farthest);
            }
        }
        HiZLevel {
            width,
            height,
            depth,
        }
    }

    /// The pixel bounds the box covers, clamped to the level; `None` when
    /// the box covers no texel.
    fn texels(&self, min: Vec2, max: Vec2) -> Option<(u32, u32, u32, u32)> {
        let lo = min.floor();
        let hi = max.ceil();
        if hi.x <= 0.0 || hi.y <= 0.0 || lo.x >= self.width as f32 || lo.y >= self.height as f32 {
            return None;
        }
        let x0 = lo.x.max(0.0) as u32;
        let y0 = lo.y.max(0.0) as u32;
        let x1 = hi.x.min(self.width as f32) as u32;
        let y1 = hi.y.min(self.height as f32) as u32;
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }

    /// The farthest depth under the box, or `None` when it covers no texel.
    pub fn max_under(&self, min: Vec2, max: Vec2) -> Option<f32> {
        let (x0, y0, x1, y1) = self.texels(min, max)?;
        let mut farthest = 0.0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                farthest = farthest.max(self.depth[(y * self.width + x) as usize]);
            }
        }
        Some(farthest)
    }
}

/// The CPU reference for [`HIZ_WGSL`]: whether each box is occluded.
pub fn occluded_cpu(level: &HiZLevel, boxes: &[HiZBox]) -> Vec<bool> {
    boxes
        .iter()
        .map(|box_| {
            level
                .max_under(box_.min, box_.max)
                .is_some_and(|farthest| farthest < box_.depth)
        })
        .collect()
}

/// The compute shader: one invocation per box, maxing the covered texels of
/// the mip and writing 1 when it is occluded.
pub const HIZ_WGSL: &str = r#"
struct Box {
    min: vec2<f32>,
    max: vec2<f32>,
    depth: f32,
    pad: f32,
}
struct Params {
    count: vec4<u32>,
    // mip level, level width, level height
    level: vec4<u32>,
}
@group(0) @binding(0) var depth_texture: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> boxes: array<Box>;
@group(0) @binding(2) var<storage, read_write> occluded: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index >= params.count.x) {
        return;
    }
    let box_ = boxes[index];
    let size = vec2<i32>(i32(params.level.y), i32(params.level.z));
    let lo = vec2<i32>(floor(box_.min));
    let hi = vec2<i32>(ceil(box_.max));
    if (hi.x <= 0 || hi.y <= 0 || lo.x >= size.x || lo.y >= size.y) {
        occluded[index] = 0u;
        return;
    }
    let lo_clamped = clamp(lo, vec2<i32>(0), size - vec2<i32>(1));
    let hi_clamped = clamp(hi, vec2<i32>(0), size);
    if (hi_clamped.x <= lo_clamped.x || hi_clamped.y <= lo_clamped.y) {
        occluded[index] = 0u;
        return;
    }
    var farthest = 0.0;
    for (var y = lo_clamped.y; y < hi_clamped.y; y = y + 1) {
        for (var x = lo_clamped.x; x < hi_clamped.x; x = x + 1) {
            farthest = max(
                farthest,
                textureLoad(depth_texture, vec2<i32>(x, y), i32(params.level.x)).r,
            );
        }
    }
    occluded[index] = select(0u, 1u, farthest < box_.depth);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn level() -> HiZLevel {
        HiZLevel {
            width: 4,
            height: 4,
            depth: vec![
                0.1, 0.2, 0.9, 0.9, //
                0.1, 0.2, 0.9, 0.9, //
                0.3, 0.3, 0.9, 0.9, //
                0.3, 0.3, 0.9, 0.9,
            ],
        }
    }

    #[test]
    fn the_pyramid_takes_the_farthest_depth() {
        let mip = level().build();
        assert_eq!(mip.width, 2);
        assert_eq!(mip.height, 2);
        assert_eq!(mip.depth, vec![0.2, 0.9, 0.3, 0.9]);
        // An odd edge repeats its last texel rather than reading out of bounds.
        let odd = HiZLevel {
            width: 3,
            height: 3,
            depth: vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9],
        }
        .build();
        assert_eq!(odd.width, 2);
        assert_eq!(odd.depth, vec![0.5, 0.6, 0.8, 0.9]);
    }

    #[test]
    fn the_cpu_reference_keeps_the_rule() {
        let level = level();
        let boxes = [
            // Behind the 0.1/0.2 near region.
            HiZBox {
                min: Vec2::new(0.0, 0.0),
                max: Vec2::new(2.0, 2.0),
                depth: 0.5,
            },
            // In front of it.
            HiZBox {
                min: Vec2::new(0.0, 0.0),
                max: Vec2::new(2.0, 2.0),
                depth: 0.15,
            },
            // Behind the far region.
            HiZBox {
                min: Vec2::new(2.0, 0.0),
                max: Vec2::new(4.0, 4.0),
                depth: 0.95,
            },
            // Outside the level.
            HiZBox {
                min: Vec2::new(9.0, 9.0),
                max: Vec2::new(11.0, 11.0),
                depth: 1.0,
            },
            // Empty.
            HiZBox {
                min: Vec2::new(1.0, 1.0),
                max: Vec2::new(1.0, 1.0),
                depth: 1.0,
            },
            // Fractional bounds cover their whole texels.
            HiZBox {
                min: Vec2::new(0.5, 0.5),
                max: Vec2::new(1.5, 1.5),
                depth: 0.5,
            },
        ];
        assert_eq!(
            occluded_cpu(&level, &boxes),
            vec![true, false, true, false, false, true]
        );
    }

    #[test]
    fn the_hiz_shader_validates() {
        let module = naga::front::wgsl::parse_str(HIZ_WGSL)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(HIZ_WGSL)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(module.entry_points[0].name, "main");
        assert_eq!(module.entry_points[0].workgroup_size, [64, 1, 1]);
        let bindings: Vec<u32> = module
            .global_variables
            .iter()
            .filter_map(|(_, var)| var.binding.as_ref().map(|b| b.binding))
            .collect();
        assert_eq!(bindings, [0, 1, 2, 3]);
    }

    #[test]
    fn the_box_layout_matches_the_shader() {
        #[repr(C)]
        struct GpuBox {
            min: [f32; 2],
            max: [f32; 2],
            depth: f32,
            pad: f32,
        }
        assert_eq!(std::mem::size_of::<GpuBox>(), 24);
        assert_eq!(std::mem::offset_of!(GpuBox, min), 0);
        assert_eq!(std::mem::offset_of!(GpuBox, max), 8);
        assert_eq!(std::mem::offset_of!(GpuBox, depth), 16);
        assert_eq!(std::mem::offset_of!(GpuBox, pad), 20);

        let module = naga::front::wgsl::parse_str(HIZ_WGSL).unwrap();
        let (_, box_) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Box"))
            .expect("Box");
        let naga::TypeInner::Struct { members, span } = &box_.inner else {
            panic!("Box is not a struct");
        };
        assert_eq!(
            members.iter().map(|m| m.offset).collect::<Vec<_>>(),
            [0, 8, 16, 20]
        );
        assert_eq!(*span, 24);
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
    fn the_hiz_pipeline_creates_and_dispatches_without_a_gpu() {
        let (device, queue) = super::super::gpu_test::noop_device();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hiz"),
            source: wgpu::ShaderSource::Wgsl(HIZ_WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hiz"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let count = 8u64;
        let boxes = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxes"),
            size: count * 24,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let occluded = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("occluded"),
            size: count * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(&(count as u32).to_le_bytes());
        bytes[16..20].copy_from_slice(&4u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&4u32.to_le_bytes());
        queue.write_buffer(&params, 0, &bytes);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hiz"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: boxes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: occluded.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
            ],
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
