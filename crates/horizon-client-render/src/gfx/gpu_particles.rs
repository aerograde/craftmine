//! E4's core: a particle buffer stepped on a compute pass, both ways.
//!
//! The CPU reference and [`PARTICLES_WGSL`] apply the same semi-implicit
//! step (gravity changes velocity, then velocity moves the position), so the
//! simulation rule is testable without a GPU; naga validates the shader and
//! wgpu's no-op device validates its pipeline and dispatch (see
//! [`gpu_test`](super::gpu_test)). Emission, collisions and rendering stay in
//! the existing particle code.
use glam::Vec3;

/// One simulated particle; position and velocity in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub position: Vec3,
    pub age: f32,
    pub velocity: Vec3,
    pub lifetime: f32,
}

/// The CPU reference for [`PARTICLES_WGSL`]: one step for every particle.
pub fn step_cpu(particles: &mut [Particle], dt: f32, gravity: f32) {
    for particle in particles {
        particle.velocity.y -= gravity * dt;
        particle.position += particle.velocity * dt;
        particle.age += dt;
    }
}

/// The compute shader: one invocation per particle, the same step.
pub const PARTICLES_WGSL: &str = r#"
struct Particle {
    position: vec3<f32>,
    age: f32,
    velocity: vec3<f32>,
    lifetime: f32,
}
struct Params {
    dt: f32,
    gravity: f32,
    count: u32,
    pad: u32,
    pad2: vec4<u32>,
}
@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var<uniform> params: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index >= params.count) {
        return;
    }
    var particle = particles[index];
    particle.velocity.y = particle.velocity.y - params.gravity * params.dt;
    particle.position = particle.position + particle.velocity * params.dt;
    particle.age = particle.age + params.dt;
    particles[index] = particle;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_reference_steps_the_particle() {
        let mut particles = [
            Particle {
                position: Vec3::ZERO,
                age: 0.0,
                velocity: Vec3::new(1.0, 2.0, 0.0),
                lifetime: 5.0,
            },
            Particle {
                position: Vec3::new(4.0, 4.0, 4.0),
                age: 1.0,
                velocity: Vec3::ZERO,
                lifetime: 2.0,
            },
        ];
        step_cpu(&mut particles, 0.5, 9.8);
        // vy = 2 - 9.8 * 0.5 = -2.9, then position += velocity * 0.5.
        assert!((particles[0].position.x - 0.5).abs() < 1e-6);
        assert!((particles[0].position.y + 1.45).abs() < 1e-6);
        assert!((particles[0].velocity.y + 2.9).abs() < 1e-6);
        assert!((particles[0].age - 0.5).abs() < 1e-6);
        // A still particle only falls and ages; lifetime is not touched here.
        assert!((particles[1].position.y - (4.0 - 4.9 * 0.5)).abs() < 1e-6);
        assert!((particles[1].age - 1.5).abs() < 1e-6);
        assert_eq!(particles[1].lifetime, 2.0);
    }

    #[test]
    fn the_particle_shader_validates() {
        let module = naga::front::wgsl::parse_str(PARTICLES_WGSL)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(PARTICLES_WGSL)));
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
        assert_eq!(bindings, [0, 1]);
    }

    #[test]
    fn the_particle_layouts_match_the_shader() {
        #[repr(C)]
        struct GpuParticle {
            position: [f32; 3],
            age: f32,
            velocity: [f32; 3],
            lifetime: f32,
        }
        assert_eq!(std::mem::size_of::<GpuParticle>(), 32);
        assert_eq!(std::mem::offset_of!(GpuParticle, position), 0);
        assert_eq!(std::mem::offset_of!(GpuParticle, age), 12);
        assert_eq!(std::mem::offset_of!(GpuParticle, velocity), 16);
        assert_eq!(std::mem::offset_of!(GpuParticle, lifetime), 28);

        let module = naga::front::wgsl::parse_str(PARTICLES_WGSL).unwrap();
        let (_, particle) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Particle"))
            .expect("Particle");
        let naga::TypeInner::Struct { members, span } = &particle.inner else {
            panic!("Particle is not a struct");
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
            [0, 4, 8, 12, 16]
        );
        assert_eq!(*span, 32);
    }

    #[test]
    fn the_particle_pipeline_creates_and_dispatches_without_a_gpu() {
        let (device, queue) = super::super::gpu_test::noop_device();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particles"),
            source: wgpu::ShaderSource::Wgsl(PARTICLES_WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("particles"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let count = 256u64;
        let particles = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particles"),
            size: count * 32,
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
        bytes[0..4].copy_from_slice(&0.016f32.to_le_bytes());
        bytes[4..8].copy_from_slice(&9.8f32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(count as u32).to_le_bytes());
        queue.write_buffer(&params, 0, &bytes);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particles"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: particles.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params.as_entire_binding(),
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
