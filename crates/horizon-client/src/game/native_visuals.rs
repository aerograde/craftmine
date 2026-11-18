use super::Game;
use horizon_client::{dynamic_light, gizmos};

fn rim_target_valid(
    loaded: bool,
    state: horizon_world::registry::BlockStateId,
    air: horizon_world::registry::BlockStateId,
) -> bool {
    loaded && state != air
}

/// The colour a light-emitting block gives off, as `rgb` and `intensity`
/// (`dynamic_light::Colored`): MiniatureXL's `CL_*_R/G/B` groups, keyed by the
/// block name that gives off that colour. Blocks not listed here keep their own
/// emission brightness but give off no colour of their own.
///
/// The list is a fixed table rather than data: the pack's colours are its
/// manifest's defaults, and reading them costs nothing at run time.
const LIGHT_COLORS: &[(&str, [f32; 3])] = &[
    ("minecraft:torch", [1.0, 0.62, 0.28]),
    ("minecraft:wall_torch", [1.0, 0.62, 0.28]),
    ("minecraft:soul_torch", [0.24, 0.78, 1.0]),
    ("minecraft:soul_wall_torch", [0.24, 0.78, 1.0]),
    ("minecraft:lantern", [1.0, 0.72, 0.38]),
    ("minecraft:soul_lantern", [0.24, 0.78, 1.0]),
    ("minecraft:campfire", [1.0, 0.55, 0.2]),
    ("minecraft:soul_campfire", [0.24, 0.78, 1.0]),
    ("minecraft:lava", [1.0, 0.38, 0.1]),
    ("minecraft:magma_block", [1.0, 0.32, 0.06]),
    ("minecraft:redstone_torch", [1.0, 0.16, 0.12]),
    ("minecraft:redstone_wall_torch", [1.0, 0.16, 0.12]),
    ("minecraft:redstone_lamp", [1.0, 0.72, 0.3]),
    ("minecraft:glowstone", [1.0, 0.82, 0.5]),
    ("minecraft:shroomlight", [1.0, 0.62, 0.3]),
    ("minecraft:sea_lantern", [0.72, 0.94, 0.98]),
    ("minecraft:amethyst_cluster", [0.62, 0.4, 1.0]),
    ("minecraft:end_rod", [1.0, 0.94, 0.86]),
    ("minecraft:froglight", [1.0, 0.9, 0.62]),
    ("minecraft:ochre_froglight", [1.0, 0.9, 0.62]),
    ("minecraft:verdant_froglight", [0.72, 1.0, 0.6]),
    ("minecraft:pearlescent_froglight", [1.0, 0.72, 0.92]),
    ("minecraft:beacon", [1.0, 0.92, 0.6]),
    ("minecraft:jack_o_lantern", [1.0, 0.66, 0.26]),
    ("minecraft:crying_obsidian", [0.6, 0.24, 1.0]),
    ("minecraft:respawn_anchor", [0.6, 0.24, 1.0]),
];

/// The colour `LIGHT_COLORS` holds for `name`, or none.
fn light_color(name: &str) -> Option<[f32; 3]> {
    // A mod's or a variant's state shares its base block's colour: `minecraft:oak_torch`
    // matches `minecraft:torch`, as do the waterlogged and axis variants. The *longest*
    // suffix wins, since `soul_torch` ends with `torch` too.
    LIGHT_COLORS
        .iter()
        .filter(|(n, _)| name.ends_with(&n["minecraft:".len()..]))
        .max_by_key(|(n, _)| n.len())
        .map(|(_, c)| *c)
}

/// How far from the camera the scan looks for light sources, in blocks: the shader's
/// `native_colored_light` fades a source out by this radius anyway, so nothing beyond it
/// would show. The cube costs `(2 * R + 1)^3` lookups: 4,913 at 8, 35,937 at 16.
const SCAN_RADIUS: i32 = 8;
/// The most sources one scan keeps, the nearest first.
const SCAN_MAX: usize = 8;

impl Game<'_> {
    /// Frame-local requests never persist after a mod is removed or a world changes.
    pub(super) fn native_frame_visuals(
        &mut self,
        camera: glam::DVec3,
        lines: &mut Vec<gizmos::Line>,
    ) -> Vec<dynamic_light::Colored> {
        let Some(player) = self.native_player() else {
            return Vec::new();
        };
        let mut rims = Vec::new();
        let mut lights = Vec::new();
        self.native_visuals(&mut rims, &mut lights);
        // No mod supplies coloured light, so the light sources around the camera are the
        // blocks that emit it. A mod's own sources win: they are listed first and the
        // shader takes the nearest `MAX` of either.
        self.scan_block_lights(camera, &mut lights);
        let close = |at: [f64; 3]| {
            at.iter().all(|v| v.is_finite())
                && (at[0] - player.x).powi(2)
                    + (at[1] - player.y).powi(2)
                    + (at[2] - player.z).powi(2)
                    <= 1024.0
        };
        let mut edges = 0;
        for rim in rims.into_iter().take(8) {
            let pos = rim.pos;
            let state = self.world.block(pos[0], pos[1], pos[2]);
            if edges >= 256
                || !rim.width.is_finite()
                || !(0.5..=4.0).contains(&rim.width)
                || !close(pos.map(|v| f64::from(v) + 0.5))
                || !rim_target_valid(
                    self.world
                        .chunks
                        .contains_key(&(pos[0].div_euclid(16), pos[2].div_euclid(16))),
                    state,
                    self.world.air,
                )
            {
                continue;
            }
            let facts = self.res.facts().get(state);
            let offset = if facts.outline_offset {
                self.pack
                    .context
                    .tables
                    .offset(state, pos[0], pos[2])
                    .unwrap_or([0.0; 3])
            } else {
                [0.0; 3]
            };
            let origin = pos.map(f64::from);
            let box_shape = self.context_outline(pos, state);
            let cube = horizon_client::blocks::cube_edges();
            let segments = if box_shape.is_some() {
                cube.as_slice()
            } else {
                facts.outline_edges.as_ref()
            };
            for edge in segments.iter().take(256 - edges) {
                let point = |end: usize| {
                    std::array::from_fn(|i| {
                        let v = edge[i + 3 * end];
                        if let Some(b) = box_shape {
                            origin[i] + b[i] + v * (b[i + 3] - b[i])
                        } else {
                            origin[i] + offset[i] + v
                        }
                    })
                };
                lines.push(gizmos::Line {
                    start: point(0),
                    end: point(1),
                    color: rim.argb,
                    width: rim.width,
                });
                edges += 1;
            }
        }
        let sources = lights.into_iter().take(8).filter(|l| {
            close(l.at)
                && l.rgb <= 0xffffff
                && l.radius.is_finite()
                && (0.25..=16.0).contains(&l.radius)
                && l.intensity.is_finite()
                && (0.0..=1.0).contains(&l.intensity)
                && self.world.chunks.contains_key(&(
                    (l.at[0].floor() as i32).div_euclid(16),
                    (l.at[2].floor() as i32).div_euclid(16),
                ))
        });
        dynamic_light::select_colored(
            sources.map(|l| (l.at, l.rgb, l.radius, l.intensity)),
            camera,
        )
    }

    /// Adds the light-emitting blocks around `camera` to `out`, the nearest [`SCAN_MAX`], so
    /// their own colour tints what they light.
    ///
    /// A fixed cube of `(2 * SCAN_RADIUS + 1)^3` block lookups, each a chunk map hash and
    /// an array index. It reruns only once the camera moves on to another block, so standing
    /// still reuses the list and walking pays for it about once a step.
    fn scan_block_lights(
        &mut self,
        camera: glam::DVec3,
        out: &mut Vec<super::native_hooks::NativeColoredLight>,
    ) {
        // The same Extras switch as the held and dropped lights: with it off nothing is
        // gathered and the shader's loop never runs, exactly as before.
        if !self.settings.extras.dynamic_lights {
            return;
        }
        let at = camera.floor().as_ivec3();
        if self.scanned_lights_at != Some(at) {
            self.scanned_lights_at = Some(at);
            self.scanned_lights.clear();
            let r = SCAN_RADIUS;
            let mut found: Vec<(f64, [f64; 3], [f32; 3], u8)> = Vec::new();
            for dy in -r..=r {
                for dz in -r..=r {
                    for dx in -r..=r {
                        let pos = at + glam::IVec3::new(dx, dy, dz);
                        let state = self.world.block(pos.x, pos.y, pos.z);
                        if state == self.world.air {
                            continue;
                        }
                        let facts = self.res.facts().get(state);
                        if facts.emission == 0 {
                            continue;
                        }
                        let Some(rgb) = light_color(&facts.name) else {
                            continue;
                        };
                        let center = pos.as_dvec3() + 0.5;
                        found.push((
                            center.distance_squared(camera),
                            [center.x, center.y, center.z],
                            rgb,
                            facts.emission,
                        ));
                    }
                }
            }
            if found.len() > SCAN_MAX {
                found.sort_by(|a, b| a.0.total_cmp(&b.0));
                found.truncate(SCAN_MAX);
            }
            for (_, at, rgb, level) in found {
                self.scanned_lights
                    .push(super::native_hooks::NativeColoredLight {
                        at,
                        rgb: ((rgb[0] * 255.0) as u32) << 16
                            | ((rgb[1] * 255.0) as u32) << 8
                            | (rgb[2] * 255.0) as u32,
                        // A torch lights about eight blocks.
                        radius: 8.0,
                        intensity: f32::from(level) / 15.0,
                    });
            }
        }
        out.extend(self.scanned_lights.iter().copied());
    }
}

#[cfg(test)]
mod tests {
    use super::{LIGHT_COLORS, SCAN_MAX, SCAN_RADIUS, light_color, rim_target_valid};
    use horizon_world::registry::BlockStateId;

    #[test]
    fn ordinary_loaded_blocks_can_have_rims() {
        let air = BlockStateId(0);
        let stone = BlockStateId(1); // No emission requirement.
        assert!(rim_target_valid(true, stone, air));
        assert!(!rim_target_valid(false, stone, air));
        assert!(!rim_target_valid(true, air, air));
    }

    /// A torch's colour is the torch's, whether or not the block is a mod's or one of
    /// vanilla's variants; a block that emits light but has no entry gives off none.
    #[test]
    fn light_colors_match_the_block_and_its_variants() {
        let torch = [1.0, 0.62, 0.28];
        assert_eq!(light_color("minecraft:torch"), Some(torch));
        assert_eq!(light_color("mymod:torch"), Some(torch));
        assert_eq!(light_color("minecraft:soul_torch"), Some([0.24, 0.78, 1.0]));
        assert_eq!(light_color("minecraft:furnace"), None);
    }

    /// The variants are found by suffix, so the longest match must win: `mymod:soul_torch`
    /// ends with both `torch` and `soul_torch`, and the soul colour is the right one.
    #[test]
    fn variants_prefer_the_longest_entry() {
        assert_eq!(
            light_color("mymod:soul_torch"),
            light_color("minecraft:soul_torch")
        );
        assert_ne!(
            light_color("mymod:soul_torch"),
            light_color("minecraft:torch")
        );
        assert_eq!(
            light_color("mymod:soul_wall_torch"),
            light_color("minecraft:soul_wall_torch")
        );
        for (name, _) in LIGHT_COLORS {
            assert_eq!(light_color(name), light_color(name), "unstable for {name}");
        }
    }

    /// The scan's cost is fixed by its radius, so it cannot grow with the world.
    #[test]
    fn the_scan_cube_stays_cheap() {
        let r = SCAN_RADIUS;
        assert!(r > 0 && r <= 16, "the cube would not be cheap");
        let lookups = (2 * r + 1).pow(3);
        assert!(lookups <= 5_000, "{lookups} block lookups per scan");
        // The shader fades a source out by this radius, so a wider scan would find
        // nothing that shows; and the shader reads at most this many.
        assert!(SCAN_MAX as i32 <= 16);
    }
}
