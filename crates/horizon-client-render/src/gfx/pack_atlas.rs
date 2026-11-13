//! The block atlas of shader packs (spec 5.1, `atlasSize` in 5.2): packs sample one 2D atlas
//! with atlas coordinates, Horizon keeps block sprites in texture arrays. With a pack, every
//! layer is copied into a grid, row by row from the top left, and an animated layer again
//! whenever its frame changes.

pub(super) struct PackAtlas {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// Sprites per row.
    pub columns: u32,
    /// A sprite's edge in pixels, at the first level.
    pub sprite: u32,
    /// Layers per sprite array.
    split: u32,
}

impl PackAtlas {
    /// Copies `layers` layers of `arrays` (`split` per array, `sprite` pixels wide, `levels`
    /// mip levels); `None` when the grid is larger than the device allows.
    pub fn new(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        arrays: &[wgpu::Texture],
        split: u32,
        layers: u32,
        sprite: u32,
    ) -> Option<Self> {
        let layers = layers.max(1);
        let columns = (f64::from(layers).sqrt().ceil() as u32).max(1);
        let rows = layers.div_ceil(columns);
        let max = device.limits().max_texture_dimension_2d;
        if columns * sprite > max || rows * sprite > max {
            return None;
        }
        // Levels past a sprite's single pixel would mix neighbours: stop there.
        let levels = arrays[0].mip_level_count().min(sprite.ilog2() + 1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shader pack block atlas"),
            size: wgpu::Extent3d {
                width: columns * sprite,
                height: rows * sprite,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let atlas = Self {
            texture,
            view,
            columns,
            sprite,
            split,
        };
        atlas.copy(encoder, arrays, 0..layers);
        Some(atlas)
    }

    /// The atlas's size in pixels (`atlasSize`).
    pub fn size(&self) -> [u32; 2] {
        [self.texture.width(), self.texture.height()]
    }

    /// Copies `layers` from the sprite arrays again, every level.
    pub fn copy(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        arrays: &[wgpu::Texture],
        layers: impl IntoIterator<Item = u32>,
    ) {
        for layer in layers {
            let Some(array) = arrays.get((layer / self.split) as usize) else {
                continue;
            };
            let (column, row) = (layer % self.columns, layer / self.columns);
            for level in 0..self.texture.mip_level_count() {
                let edge = self.sprite >> level;
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: array,
                        mip_level: level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer % self.split,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: level,
                        origin: wgpu::Origin3d {
                            x: column * edge,
                            y: row * edge,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: edge,
                        height: edge,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
    }
}

/// A `TerrainVertex` of flat draws' arena page (`hz_vertices`, six words each) into the vertex
/// inputs, for vertex `hz_vertex_index`.
pub const PULL: &str = "        uint hz_pw = hz_vertex_index * 6u;\n        \
    vec3 hz_pull_pos = uintBitsToFloat(uvec3(hz_vertices[hz_pw], hz_vertices[hz_pw + 1u], hz_vertices[hz_pw + 2u]));\n        \
    hz_v_pos = uvec4(uvec3(round((hz_pull_pos - vec3(-8.0)) * 2048.0)), 0u);\n        \
    hz_v_uv = uvec2(hz_vertices[hz_pw + 3u] & 65535u, hz_vertices[hz_pw + 3u] >> 16u);\n        \
    hz_v_layer = hz_vertices[hz_pw + 4u] & 65535u;\n        \
    uint hz_pl = hz_vertices[hz_pw + 4u] >> 16u;\n        \
    hz_v_light = uvec2(hz_pl & 255u, hz_pl >> 8u);\n        \
    hz_v_color = unpackUnorm4x8(hz_vertices[hz_pw + 5u]);\n";

/// GLSL: the face normal and tangent of a flat-drawn vertex, from its quad's first three corners
/// (quads are four consecutive vertices of the page; position words 0-2, UV word 3).
const FACE: &str = "    uint hz_q = (uint(gl_VertexIndex) & ~3u) * 6u;\n    \
    vec3 hz_p0 = uintBitsToFloat(uvec3(hz_vertices[hz_q], hz_vertices[hz_q + 1u], hz_vertices[hz_q + 2u]));\n    \
    vec3 hz_p1 = uintBitsToFloat(uvec3(hz_vertices[hz_q + 6u], hz_vertices[hz_q + 7u], hz_vertices[hz_q + 8u]));\n    \
    vec3 hz_p2 = uintBitsToFloat(uvec3(hz_vertices[hz_q + 12u], hz_vertices[hz_q + 13u], hz_vertices[hz_q + 14u]));\n    \
    uvec3 hz_w = uvec3(hz_vertices[hz_q + 3u], hz_vertices[hz_q + 9u], hz_vertices[hz_q + 15u]);\n    \
    vec2 hz_t0 = vec2(hz_w.x & 65535u, hz_w.x >> 16u);\n    \
    vec2 hz_d1 = vec2(hz_w.y & 65535u, hz_w.y >> 16u) - hz_t0;\n    \
    vec2 hz_d2 = vec2(hz_w.z & 65535u, hz_w.z >> 16u) - hz_t0;\n    \
    vec3 hz_e1 = hz_p1 - hz_p0;\n    \
    vec3 hz_e2 = hz_p2 - hz_p0;\n    \
    vec3 hz_n = cross(hz_e1, hz_e2);\n    \
    if (dot(hz_n, hz_n) > 1e-12) {\n        \
        hz_Normal = normalize(hz_n);\n        \
        float hz_r = hz_d1.x * hz_d2.y - hz_d2.x * hz_d1.y;\n        \
        vec3 hz_t = (hz_e1 * hz_d2.y - hz_e2 * hz_d1.y) * sign(hz_r);\n        \
        vec3 hz_b = (hz_e2 * hz_d1.x - hz_e1 * hz_d2.x) * sign(hz_r);\n        \
        if (dot(hz_t, hz_t) > 1e-12) {\n            \
            hz_t = normalize(hz_t - hz_Normal * dot(hz_Normal, hz_t));\n            \
            hz_tangent = vec4(hz_t, dot(cross(hz_t, hz_Normal), hz_b) < 0.0 ? -1.0 : 1.0);\n        \
        }\n    \
    }\n";

impl PackAtlas {
    /// The vertex inputs of a gbuffers program drawing terrain (`TerrainVertex`, terrain.wgsl's
    /// `vs_terrain` or, for `flat` draws, `vs_terrain_flat`), in atlas coordinates.
    pub fn terrain_inputs(&self, flat: bool, far_lod: bool) -> horizon_shaders::GbuffersInputs {
        let mut declarations = String::from(
            "layout(location = 0) in uvec4 hz_v_pos;\nlayout(location = 1) in uvec2 hz_v_uv;\n\
             layout(location = 2) in uint hz_v_layer;\nlayout(location = 4) in uvec2 hz_v_light;\n\
             layout(location = 3) in vec4 hz_v_color;\n",
        );
        let origin = if flat {
            declarations.push_str(
                "layout(set = 1, binding = 0) readonly buffer HzBlockSlots { uint hz_block_slots[]; };\n\
                 layout(set = 1, binding = 1) readonly buffer HzSlotOrigins { vec4 hz_slot_origins[]; };\n",
            );
            // `mc_Entity` of each sprite layer: id, then 1 for fluids or -1 (the game's table),
            // and the arena page's vertices (`TerrainVertex`, six words each).
            declarations.push_str(
                "layout(set = 3, binding = 0) readonly buffer HzBlockIds { int hz_block_ids[]; };\n\
                 layout(set = 3, binding = 1) readonly buffer HzVertices { uint hz_vertices[]; };\n",
            );
            "hz_slot_origins[hz_block_slots[uint(gl_VertexIndex) >> 8u]]"
        } else {
            declarations.push_str("layout(location = 5) in vec4 hz_v_origin;\n");
            "hz_v_origin"
        };
        let [width, height] = self.size();
        let scale = [
            self.sprite as f32 / width as f32,
            self.sprite as f32 / height as f32,
        ];
        let columns = self.columns;
        // UVs past a sprite (models up to 2, distant tiles) cannot repeat in an atlas: clamped.
        // The colour's alpha holds HDR's emission level (`mesher::EMISSION_BITS`): packs get 1.
        // `TerrainVertex` (mesher.rs, terrain.wgsl's `vs_terrain`): a section vertex is
        // 1/2048-block fixed point with tag 0; a distant tile is 1/16-block with tag 1 or
        // 1/128-block with tag 2, all from -64. The raw vertex is the game's buffer, so the
        // declarations above must be its formats (`Uint16x4`, ..., `Unorm8x4`).
        let body = format!(
            "    vec4 hz_origin = {origin};\n    \
             float hz_pos_scale = hz_v_pos.w == 2u ? (1.0 / 128.0) : (hz_v_pos.w == 1u ? 16.0 : 2048.0);\n    \
             float hz_pos_min = (hz_v_pos.w == 1u || hz_v_pos.w == 2u) ? -64.0 : -8.0;\n    \
             vec3 hz_pos = vec3(hz_v_pos.xyz) / hz_pos_scale + hz_pos_min;\n    \
             float hz_uv_scale = hz_v_pos.w == 0u ? 32768.0 : 32.0;\n    \
             uint hz_layer = hz_v_layer & 32767u;\n    \
             hz_cell = vec2(float(hz_layer % {columns}u), float(hz_layer / {columns}u));\n    \
             hz_scale = vec2({:?}, {:?});\n    \
             hz_Vertex = vec4(hz_pos + hz_origin.xyz, 1.0);\n    \
             hz_MultiTexCoord0 = vec4((hz_cell + clamp((vec2(hz_v_uv) / hz_uv_scale) * (hz_origin.w > 0.0 ? uintBitsToFloat(floatBitsToUint(hz_origin.w) & 0xff800000u) : fract(abs(hz_origin.w))), 0.0, 1.0)) * hz_scale, 0.0, 1.0);\n    \
             hz_MultiTexCoord1 = vec4(float(hz_v_light.y), float(hz_v_light.x), 0.0, 1.0);\n    \
             hz_MultiTexCoord2 = hz_MultiTexCoord1;\n    \
             hz_Color = vec4(hz_v_color.rgb, 1.0);\n    \
             hz_Normal = vec3(0.0, 1.0, 0.0);\n    \
             hz_tangent = vec4(1.0, 0.0, 0.0, 1.0);\n",
            scale[0], scale[1],
        );
        let mut body = body;
        if flat {
            body.push_str(FACE);
        }
        declarations.push_str("vec2 hz_cell; vec2 hz_scale; vec4 hz_tangent;\n");
        let entity = if flat {
            "vec4(float(hz_block_ids[2u * hz_layer]), float(hz_block_ids[2u * hz_layer + 1u]), 0.0, 0.0)"
        } else {
            "vec4(-1.0, -1.0, 0.0, 0.0)"
        };
        // Distant LOD tiles (the mesher sets 0x4000 on their layer) get a material id the
        // gbuffer's cheap path recognises, so the pack does not run its full PBR/parallax path
        // on thousands of far tiles.
        let entity = if far_lod {
            format!("((hz_v_layer & 0x4000u) != 0u ? vec4(4000.0, 0.0, 0.0, 0.0) : {entity})")
        } else {
            entity.to_owned()
        };
        let fragment_before = if far_lod {
            "    if (materialID >= 4000) {\n        \
             color = vec4(glcolor.rgb, 1.0);\n        \
             outData1 = vec4(encodeNormal(vec3(0.0, 1.0, 0.0)), lmcoord.y, 0.0);\n        \
             return;\n    }\n"
                .to_owned()
        } else {
            String::new()
        };
        // The block a face belongs to: half a block behind its centre (the quad's diagonal).
        let mid_block = if flat {
            "vec4((floor((hz_p0 + hz_p2) * 0.5 - 0.5 * hz_Normal) + 0.5 - hz_pos) * 64.0, 0.0)"
        } else {
            "vec4(0.0)"
        };
        let attributes = [
            ("mc_Entity", entity),
            ("at_midBlock", mid_block.to_owned()),
            (
                "mc_midTexCoord",
                "vec4((hz_cell + 0.5) * hz_scale, 0.0, 1.0)".to_owned(),
            ),
            ("at_tangent", "hz_tangent".to_owned()),
            ("vaPosition", "hz_Vertex.xyz".to_owned()),
            ("vaUV0", "hz_MultiTexCoord0.xy".to_owned()),
            ("vaUV2", "hz_MultiTexCoord1.xy".to_owned()),
            ("vaColor", "hz_Color".to_owned()),
            ("vaNormal", "hz_Normal".to_owned()),
        ];
        horizon_shaders::GbuffersInputs {
            declarations,
            body,
            attributes: attributes
                .iter()
                .map(|(n, v)| ((*n).to_owned(), v.clone()))
                .collect(),
            after: String::new(),
            fragment_before,
            alpha_test: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two arrays of 2x2 sprites, one layer split; each layer a colour of its own.
    #[test]
    fn layers_land_in_their_cells() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let (split, layers, sprite) = (3u32, 5u32, 2u32);
        let arrays: Vec<wgpu::Texture> = (0..2)
            .map(|a| {
                let count = (layers - a * split).min(split);
                let pixels: Vec<u8> = (0..count)
                    .flat_map(|l| [(a * split + l) as u8 * 40, 0, 0, 255].repeat(4))
                    .collect();
                wgpu::util::DeviceExt::create_texture_with_data(
                    &device,
                    &queue,
                    &wgpu::TextureDescriptor {
                        label: None,
                        size: wgpu::Extent3d {
                            width: sprite,
                            height: sprite,
                            depth_or_array_layers: count,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &pixels,
                )
            })
            .collect();
        let mut encoder = device.create_command_encoder(&Default::default());
        let atlas = PackAtlas::new(&device, &mut encoder, &arrays, split, layers, sprite).unwrap();
        assert_eq!((atlas.columns, atlas.size()), (3, [6, 4]));
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            atlas.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: 6,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = read.slice(..).get_mapped_range().unwrap();
        let red = |x: usize, y: usize| data[y * 256 + x * 4];
        // Layer 4 (second array, its second layer) is in row 1, column 1.
        assert_eq!([red(0, 0), red(2, 0), red(4, 0)], [0, 40, 80]);
        assert_eq!([red(1, 3), red(3, 2), red(5, 2)], [120, 160, 0]);
    }
}
