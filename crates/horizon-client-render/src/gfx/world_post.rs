//! Resolution scale, FXAA and TAA (Horizon settings): the world renders into its
//! own target at the scaled size, then one full-screen pass copies or
//! anti-aliases it onto the screen, under the hand and HUD. With none on,
//! the world renders straight to the screen.
//!
//! The Miniature Shader profile's glow runs here too, at a quarter of the
//! world's pixels: a bright pass (threshold and light shafts), a separable
//! Gaussian, and one composite that applies it with the grade. The glow was a
//! ring of full-resolution taps; a quarter-size chain spreads wider for a
//! fraction of the samples.
//!
//! TAA jitters the world's projection by a sub-pixel offset each frame and blends
//! the frame into a history reprojected through the depth buffer: its motion
//! vectors are the camera's, so what moves on its own leans on the neighbourhood
//! clamp alone.

use glam::{DMat4, DVec3, Mat4, Vec2};

/// The TAA history format: 8 bits would band as frames accumulate.
const HISTORY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The glow chain's format: wide enough that a bright sun does not clamp.
const BLOOM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

struct WorldTarget {
    size: [u32; 2],
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    group: wgpu::BindGroup,
    taa: Option<History>,
    bloom: Option<Bloom>,
}

/// The quarter-resolution glow: the bright pass, its horizontal blur and the
/// vertical one that lands in `blurred` (what the composite reads).
struct Bloom {
    bright: wgpu::TextureView,
    temp: wgpu::TextureView,
    blurred: wgpu::TextureView,
    /// The world colour, depth and profile the bright pass reads.
    bright_pass: wgpu::BindGroup,
    /// Horizontal (from `bright`) and vertical (from `temp`).
    blur: [wgpu::BindGroup; 2],
    direction: [wgpu::Buffer; 2],
}

/// The same pipelines and layouts for every target size.
struct BloomPasses {
    bright_layout: wgpu::BindGroupLayout,
    blur_layout: wgpu::BindGroupLayout,
    bright: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
}

/// TAA's two history textures at the output size (the world's, times the upscale), written
/// in turn: `resolve[i]` writes `history[i]` from the world and `history[1 - i]`; `show[i]`
/// copies `history[i]` to the screen.
struct History {
    size: [u32; 2],
    history: [wgpu::TextureView; 2],
    resolve: [wgpu::BindGroup; 2],
    show: [wgpu::BindGroup; 2],
}

/// `world_post.wgsl`'s `Taa`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TaaUniform {
    /// This frame's unjittered clip space to the previous frame's.
    reproject: [[f32; 4]; 4],
    /// xy: this frame's jitter in clip space; z: 1 when the history is unusable.
    jitter: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VisualUniform {
    params: [f32; 4],
    extra: [f32; 4],
    /// x: daylight (0 night, 1 noon), y: dawn/dusk, z/w: the sun's screen position.
    mood: [f32; 4],
    /// x: exposure.
    post: [f32; 4],
}

/// TAA's GPU resources, absent when the device cannot compile the shader
/// (the OpenGL backend's naga GLSL emitter does not support `textureLoad`
/// from depth textures).
struct TaaResources {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
}

pub struct Post {
    /// The composite and present pipelines' bindings: the world, its sampler,
    /// the glow and the profile.
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    copy: wgpu::RenderPipeline,
    present: wgpu::RenderPipeline,
    fxaa: wgpu::RenderPipeline,
    format: wgpu::TextureFormat,
    /// World resolution relative to the window (0.5..=2.0).
    pub scale: f32,
    pub antialias: bool,
    pub taa: bool,
    /// Output pixels per rendered pixel while TAA is on (1: native): the world renders
    /// this much smaller and TAA rebuilds it at the resolution scale's size.
    pub upscale: f32,
    /// TAA runs this frame (`jitter`): not on the title screen or under a shader pack.
    taa_on: bool,
    target: Option<WorldTarget>,
    taa_resources: Option<TaaResources>,
    /// Frames jittered so far (the Halton index), and which history the next resolve writes.
    taa_frame: u32,
    /// The last TAA frame's unjittered `view_proj` and camera, `None` once a frame ran without it.
    taa_previous: Option<(DMat4, DVec3)>,
    taa_this_frame: TaaUniform,
    visual: wgpu::Buffer,
    /// The profile is on this frame, and how strong its glow is (the tier's).
    visual_on: bool,
    bloom_strength: f32,
    bloom_passes: BloomPasses,
    /// Bound where a pass has no glow of its own.
    blank: wgpu::TextureView,
}

impl Post {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Horizon post"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../world_post.wgsl").into()),
        });
        let texture = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let sampler = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let float = wgpu::TextureSampleType::Float { filterable: true };
        // 0 world, 1 sampler, 3 glow, 5 the profile.
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Horizon post bindings"),
            entries: &[texture(0, float), sampler(1), texture(3, float), uniform(5)],
        });
        // The bright pass: 0 world, 1 sampler, 2 depth, 5 the profile.
        let bright_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Horizon glow bright pass"),
            entries: &[
                texture(0, float),
                sampler(1),
                texture(2, wgpu::TextureSampleType::Depth),
                uniform(5),
            ],
        });
        // One blur direction at a time: 0 source, 1 sampler, 5 the profile, 7 direction.
        let blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Horizon glow blur"),
            entries: &[texture(0, float), sampler(1), uniform(5), uniform(7)],
        });
        let pipeline_layout = |label: &str, layouts: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: layouts,
                ..Default::default()
            })
        };
        let post_layout = pipeline_layout("Horizon post", &[Some(&layout)]);
        let bright_layout_owned =
            pipeline_layout("Horizon glow bright pass", &[Some(&bright_layout)]);
        let blur_layout_owned = pipeline_layout("Horizon glow blur", &[Some(&blur_layout)]);
        let pipeline = |layout: &wgpu::PipelineLayout, entry: &str, format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Horizon post"),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_post"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        // The OpenGL backend's GLSL translator does not support textureLoad
        // from depth textures: on devices running through OpenGL (such as older
        // Intel GPUs on Linux), TAA cannot compile its pipeline.
        let taa_resources = {
            let taa_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Horizon TAA bindings"),
                entries: &[
                    texture(0, float),
                    sampler(1),
                    texture(2, wgpu::TextureSampleType::Depth),
                    texture(3, float),
                    uniform(4),
                    uniform(5),
                    texture(6, float),
                ],
            });
            let taa_pipeline_layout = pipeline_layout("Horizon TAA", &[Some(&taa_layout)]);
            let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
            let taa_pipeline = pipeline(&taa_pipeline_layout, "fs_taa", HISTORY_FORMAT);
            let internal = horizon_os::block_on(internal.pop()).flatten();
            match internal.or(horizon_os::block_on(validation.pop()).flatten()) {
                None => {
                    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("Horizon TAA"),
                        size: std::mem::size_of::<TaaUniform>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    Some(TaaResources {
                        layout: taa_layout,
                        pipeline: taa_pipeline,
                        uniform,
                    })
                }
                Some(e) => {
                    log::warn!("Horizon renderer: temporal anti-aliasing unavailable ({e})");
                    None
                }
            }
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Horizon post"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let visual = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Horizon visual profile"),
            size: std::mem::size_of::<VisualUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Horizon post blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Self {
            copy: pipeline(&post_layout, "fs_copy", format),
            present: pipeline(&post_layout, "fs_present", format),
            fxaa: pipeline(&post_layout, "fs_fxaa", format),
            bloom_passes: BloomPasses {
                bright_layout,
                blur_layout,
                bright: pipeline(&bright_layout_owned, "fs_bloom_bright", BLOOM_FORMAT),
                blur: pipeline(&blur_layout_owned, "fs_bloom_blur", BLOOM_FORMAT),
            },
            layout,
            sampler,
            format,
            scale: 1.0,
            antialias: false,
            taa: false,
            upscale: 1.0,
            taa_on: false,
            target: None,
            taa_resources,
            taa_frame: 0,
            taa_previous: None,
            taa_this_frame: bytemuck::Zeroable::zeroed(),
            visual,
            visual_on: false,
            bloom_strength: 0.0,
            blank,
        }
    }

    /// Whether TAA is supported on this GPU/backend.
    pub fn supports_taa(&self) -> bool {
        self.taa_resources.is_some()
    }

    /// The world's rendered size and the size it is shown at (TAA's history).
    fn sizes(&self, window: [u32; 2]) -> ([u32; 2], [u32; 2]) {
        let output = window.map(|v| ((v as f32 * self.scale).round() as u32).clamp(1, 8192));
        let upscale = if self.taa_on { self.upscale } else { 1.0 };
        (
            output.map(|v| ((v as f32 / upscale).round() as u32).max(1)),
            output,
        )
    }

    /// TAA's sub-pixel jitter this frame, as a clip-space matrix to multiply into the
    /// world's projection (identity while `on` is false); `view_proj` is the unjittered
    /// projection, relative to `camera`. Call it before `prepare`, once per frame.
    pub fn jitter(&mut self, window: [u32; 2], on: bool, view_proj: Mat4, camera: DVec3) -> Mat4 {
        self.taa_on = on && self.taa && self.taa_resources.is_some();
        if !self.taa_on {
            self.taa_previous = None;
            return Mat4::IDENTITY;
        }
        self.taa_frame = self.taa_frame.wrapping_add(1);
        let (size, _) = self.sizes(window);
        // Halton (2, 3) over 8 frames per output pixel (FSR's count), in rendered
        // pixels around the centre.
        let phases = (8.0 * self.upscale * self.upscale).ceil() as u32;
        let i = self.taa_frame % phases + 1;
        let offset = Vec2::new(halton(i, 2), halton(i, 3)) - 0.5;
        let jitter = offset * 2.0 / Vec2::new(size[0] as f32, size[1] as f32);
        let current = view_proj.as_dmat4();
        let (reproject, reset) = match self.taa_previous {
            Some((previous, previous_camera)) => (
                reprojection(previous, previous_camera, current, camera),
                0.0,
            ),
            None => (DMat4::IDENTITY, 1.0),
        };
        self.taa_previous = Some((current, camera));
        self.taa_this_frame = TaaUniform {
            reproject: reproject.as_mat4().to_cols_array_2d(),
            jitter: [jitter.x, jitter.y, reset, 0.0],
        };
        Mat4::from_translation(jitter.extend(0.0))
    }

    /// The world's colour and depth targets this frame, or `None` to draw
    /// it straight to the screen.
    /// `always`: a target even at scale 1 without anti-aliasing (a shader pack reads it).
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        window: [u32; 2],
        always: bool,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        if self.scale == 1.0 && !self.antialias && !self.taa_on && !always {
            self.target = None;
            return None;
        }
        let (size, output) = self.sizes(window);
        let history_size = self.taa_on.then_some(output);
        let bloom = self.visual_on && self.bloom_strength > 0.0;
        if self.target.as_ref().is_none_or(|t| {
            t.size != size
                || t.taa.as_ref().map(|h| h.size) != history_size
                || (bloom && t.bloom.is_none())
                || (!bloom && t.bloom.is_some())
        }) {
            let make_tex = |label, size: [u32; 2], format, usage| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
            };
            // COPY_SRC: a native pack's material stage reads copies of them (`opaque_depth`,
            // `opaque_color`).
            let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC;
            let color = make_tex("Scaled world", size, self.format, sampled)
                .create_view(&Default::default());
            // TEXTURE_BINDING: occlusion culling, the glow's bright pass and TAA read it.
            let depth = make_tex(
                "Scaled world depth",
                size,
                wgpu::TextureFormat::Depth32Float,
                sampled | wgpu::TextureUsages::COPY_SRC,
            )
            .create_view(&Default::default());
            let glow = bloom.then(|| self.bloom_textures(device, &color, &depth, size));
            let glow_view = glow.as_ref().map_or(&self.blank, |b| &b.blurred);
            let group = self.bind(device, &color, glow_view);
            let taa = history_size.and_then(|output| {
                let res = self.taa_resources.as_ref()?;
                let history = [0, 1].map(|_| {
                    make_tex("TAA history", output, HISTORY_FORMAT, sampled)
                        .create_view(&Default::default())
                });
                let resolve = [0, 1].map(|i| {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Horizon TAA"),
                        layout: &res.layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&color),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&self.sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(&depth),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::TextureView(glow_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 4,
                                resource: res.uniform.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 5,
                                resource: self.visual.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 6,
                                resource: wgpu::BindingResource::TextureView(&history[1 - i]),
                            },
                        ],
                    })
                });
                let show = [0, 1].map(|i| self.bind(device, &history[i], glow_view));
                Some(History {
                    size: output,
                    history,
                    resolve,
                    show,
                })
            });
            // New textures: no history yet.
            self.taa_this_frame.jitter[2] = 1.0;
            self.target = Some(WorldTarget {
                size,
                color,
                depth,
                group,
                taa,
                bloom: glow,
            });
        }
        self.target
            .as_ref()
            .map(|t| (t.color.clone(), t.depth.clone()))
    }

    /// The glow chain at a quarter of the world's size, bound to `color` and `depth`.
    fn bloom_textures(
        &self,
        device: &wgpu::Device,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        size: [u32; 2],
    ) -> Bloom {
        let half = size.map(|v| (v / 4).max(1));
        let make = |label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: half[0],
                        height: half[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: BLOOM_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let bright = make("Horizon glow bright");
        let temp = make("Horizon glow temp");
        let blurred = make("Horizon glow");
        let bright_pass = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Horizon glow bright pass"),
            layout: &self.bloom_passes.bright_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(color),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(depth),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.visual.as_entire_binding(),
                },
            ],
        });
        let direction = [0, 1].map(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Horizon glow direction"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let blur = [&bright, &temp]
            .into_iter()
            .enumerate()
            .map(|(i, source)| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Horizon glow blur"),
                    layout: &self.bloom_passes.blur_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(source),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: self.visual.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 7,
                            resource: direction[i].as_entire_binding(),
                        },
                    ],
                })
            })
            .collect::<Vec<_>>()
            .try_into()
            .expect("two blur directions");
        Bloom {
            bright,
            temp,
            blurred,
            bright_pass,
            blur,
            direction,
        }
    }

    /// The size of the world's own target, when it has one.
    pub fn size(&self) -> Option<[u32; 2]> {
        self.target.as_ref().map(|t| t.size)
    }

    pub fn set_visual(
        &mut self,
        queue: &wgpu::Queue,
        params: [f32; 8],
        mood: [f32; 4],
        post: [f32; 4],
    ) {
        self.visual_on = params[0] > 0.5;
        self.bloom_strength = params[4];
        queue.write_buffer(
            &self.visual,
            0,
            bytemuck::bytes_of(&VisualUniform {
                params: [params[0], params[1], params[2], params[3]],
                extra: [params[4], params[5], params[6], params[7]],
                mood,
                post,
            }),
        );
    }

    /// Bindings that copy (or anti-alias) `view` onto the screen in `finish_with`.
    pub fn bind(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Horizon post"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(bloom),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.visual.as_entire_binding(),
                },
            ],
        })
    }

    /// Draw the scaled world onto `screen`, through TAA when this frame's `jitter` had it on.
    pub fn finish(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        screen: &wgpu::TextureView,
    ) {
        let Some(target) = &self.target else { return };
        // A target built while the profile glowed keeps its chain: skip it while
        // the profile is off or the tier has no glow, rather than clearing three
        // textures a frame for nothing.
        if let Some(bloom) = &target.bloom
            && self.visual_on
            && self.bloom_strength > 0.0
        {
            self.bloom(queue, encoder, bloom);
        }
        let Some(taa) = target.taa.as_ref().filter(|_| self.taa_previous.is_some()) else {
            self.finish_with(encoder, screen, &target.group);
            return;
        };
        let Some(res) = &self.taa_resources else {
            self.finish_with(encoder, screen, &target.group);
            return;
        };
        let i = (self.taa_frame % 2) as usize;
        queue.write_buffer(&res.uniform, 0, bytemuck::bytes_of(&self.taa_this_frame));
        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: &taa.history[i],
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Horizon TAA"),
            color_attachments: &attachments,
            ..Default::default()
        });
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, &taa.resolve[i], &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        // The history already carries the profile: showing it must not grade twice.
        self.present_with(encoder, screen, &taa.show[i]);
    }

    /// The bright pass and its two blur directions, into the chain's textures.
    fn bloom(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, bloom: &Bloom) {
        for (i, view) in [&bloom.bright, &bloom.temp, &bloom.blurred]
            .into_iter()
            .enumerate()
        {
            if i == 0 {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Horizon glow bright pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&self.bloom_passes.bright);
                pass.set_bind_group(0, &bloom.bright_pass, &[]);
                pass.draw(0..3, 0..1);
            } else {
                let direction: [f32; 4] = if i == 1 {
                    [1.0, 0.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0, 0.0]
                };
                queue.write_buffer(&bloom.direction[i - 1], 0, bytemuck::bytes_of(&direction));
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Horizon glow blur"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&self.bloom_passes.blur);
                pass.set_bind_group(0, &bloom.blur[i - 1], &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }

    /// Draw what `group` binds (`bind`) onto `screen`.
    pub fn finish_with(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        screen: &wgpu::TextureView,
        group: &wgpu::BindGroup,
    ) {
        let pipeline = if self.antialias {
            &self.fxaa
        } else {
            &self.copy
        };
        self.draw(encoder, screen, group, pipeline);
    }

    /// `fs_present`: what TAA's resolve already graded, copied ungraded.
    fn present_with(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        screen: &wgpu::TextureView,
        group: &wgpu::BindGroup,
    ) {
        self.draw(encoder, screen, group, &self.present);
    }

    fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        screen: &wgpu::TextureView,
        group: &wgpu::BindGroup,
        pipeline: &wgpu::RenderPipeline,
    ) {
        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: screen,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Horizon post"),
            color_attachments: &attachments,
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// This frame's clip space to the previous frame's, for camera-relative projections:
/// a point `camera - previous_camera` further from the previous camera than from this one.
fn reprojection(previous: DMat4, previous_camera: DVec3, current: DMat4, camera: DVec3) -> DMat4 {
    previous * DMat4::from_translation(camera - previous_camera) * current.inverse()
}

/// The radical inverse of `i` in `base`: a low-discrepancy sequence in 0..1.
fn halton(mut i: u32, base: u32) -> f32 {
    let (mut f, mut r) = (1.0, 0.0);
    while i > 0 {
        f /= base as f32;
        r += f * (i % base) as f32;
        i /= base;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reprojection_finds_where_a_block_was_last_frame() {
        let projection = DMat4::perspective_rh(1.2, 16.0 / 9.0, 1024.0, 0.05);
        let previous = projection * DMat4::from_rotation_y(0.3);
        let current = projection * DMat4::from_rotation_y(0.35);
        let (previous_camera, camera) = (DVec3::new(10.0, 64.0, 5.0), DVec3::new(10.5, 64.2, 4.0));
        let block = DVec3::new(14.0, 66.0, -8.0);
        let clip = |m: DMat4, eye: DVec3| m * (block - eye).extend(1.0);
        let now = clip(current, camera);
        let then = reprojection(previous, previous_camera, current, camera) * (now / now.w);
        let expected = clip(previous, previous_camera);
        assert!((then.truncate() / then.w - expected.truncate() / expected.w).length() < 1e-9);
    }

    #[test]
    fn jitter_stays_inside_the_pixel() {
        for i in 1..=8 {
            let (x, y) = (halton(i, 2), halton(i, 3));
            assert!((0.0..1.0).contains(&x) && (0.0..1.0).contains(&y));
        }
        assert_eq!(
            [halton(1, 2), halton(2, 2), halton(3, 2)],
            [0.5, 0.25, 0.75]
        );
    }

    fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        pollster::block_on(adapter.request_device(&Default::default())).ok()
    }

    #[test]
    fn world_post_pipelines_compile_or_fall_back() {
        let Some((device, _)) = test_device() else {
            return;
        };
        let post = Post::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        // Copy, present, FXAA and the glow chain always compile; TAA compiles if
        // the backend supports it.
        let _ = post.supports_taa();
    }

    #[test]
    fn the_glow_chain_runs_and_the_composite_keeps_the_world() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let size = [64u32, 64];
        let mut post = Post::new(&device, format);
        post.set_visual(
            &queue,
            [1.0, 1.06, 1.14, 0.22, 0.55, 0.82, 10.0, 0.06],
            [1.0, 0.0, 0.5, 0.5],
            [1.02, 0.0, 0.0, 0.0],
        );
        let Some((color, _)) = post.prepare(&device, size, true) else {
            return;
        };
        let screen = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("post test screen"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = screen.create_view(&Default::default());
        // A flat picture below the glow threshold stays neutral and passes through;
        // a white one gains the glow and clips at white.
        for (grey, expected) in [(0.5f64, 100..=180), (1.0, 250..=255)] {
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                // The clear alone: the pass ends when the guard drops.
                let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("post test world"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &color,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: grey,
                                g: grey,
                                b: grey,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
            }
            post.finish(&queue, &mut encoder, &view);
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("post test read"),
                size: u64::from(size[0] * size[1] * 4),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                screen.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &read,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(size[0] * 4),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            read.slice(..)
                .map_async(wgpu::MapMode::Read, |r| r.unwrap());
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let mapped = read.slice(..).get_mapped_range().unwrap();
            let centre = ((size[1] / 2 * size[0] + size[0] / 2) * 4) as usize;
            let pixel = [mapped[centre], mapped[centre + 1], mapped[centre + 2]];
            assert!(
                expected.contains(&pixel[0]),
                "grey {grey}: {pixel:?} outside {expected:?}"
            );
            assert!(
                pixel.iter().all(|c| u8::abs_diff(*c, pixel[0]) <= 8),
                "grey {grey} must stay neutral: {pixel:?}"
            );
        }
    }

    #[test]
    fn the_taa_resolve_runs_with_the_profile() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let size = [64u32, 64];
        let mut post = Post::new(&device, format);
        if !post.supports_taa() {
            return;
        }
        post.taa = true;
        post.set_visual(
            &queue,
            [1.0, 1.06, 1.14, 0.22, 0.55, 0.82, 10.0, 0.06],
            [1.0, 0.0, 0.5, 0.5],
            [1.02, 0.0, 0.0, 0.0],
        );
        // Jitter before prepare, as a frame does. The first jittered frame has no
        // history: the resolve returns this frame and the present shows it.
        post.jitter(size, true, Mat4::IDENTITY, DVec3::ZERO);
        let Some((color, _)) = post.prepare(&device, size, true) else {
            return;
        };
        let screen = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("taa test screen"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = screen.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("taa test world"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.15,
                            g: 0.6,
                            b: 0.15,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        post.finish(&queue, &mut encoder, &view);
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("taa test read"),
            size: u64::from(size[0] * size[1] * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            screen.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size[0] * 4),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        read.slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let mapped = read.slice(..).get_mapped_range().unwrap();
        let centre = ((size[1] / 2 * size[0] + size[0] / 2) * 4) as usize;
        let pixel = [mapped[centre], mapped[centre + 1], mapped[centre + 2]];
        assert!(
            pixel[1] > pixel[0] && pixel[1] > pixel[2] && pixel[1] > 100,
            "the resolved frame shows the world: {pixel:?}"
        );
    }

    #[test]
    fn the_post_shader_validates() {
        let module =
            naga::front::wgsl::parse_str(include_str!("../world_post.wgsl")).expect("WGSL parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("WGSL validates");
    }
}
