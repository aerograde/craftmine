use crate::graphics::VisualQuality;

/// One of the three quality tiers behind the built-in visual profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    High,
    Medium,
    Low,
}

impl Tier {
    pub fn from_quality(quality: VisualQuality) -> Self {
        match quality {
            VisualQuality::Auto | VisualQuality::High => Self::High,
            VisualQuality::Medium => Self::Medium,
            VisualQuality::Low => Self::Low,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    pub const fn shadow(self) -> ShadowPlan {
        // One orthographic map over the near field. `taps` only switches the
        // filter on (the shader tests it against 0.5): every tier runs the same
        // five-sample kernel, four on a rotated disc about the fragment. A
        // single compare gives a shadow edge with hard stair-steps a block
        // wide, which is what "the shadows look wrong" was. Five compares once
        // per terrain pixel is small beside the terrain shader itself, and the
        // cheaper tiers already pay for the filter with a smaller map rather
        // than with a jagged edge.
        //
        // `distance` must keep the shadow camera's 7-block snap on a whole
        // number of texels, or every redraw shifts the map by a fraction of a
        // texel and the shadow visibly slides when anything in the scene
        // changes — placing one block did it. Texel = 2 * distance / resolution,
        // so 128 on 1024 gives 0.25 blocks and 7 / 0.25 = 28 texels exactly.
        // 160 gives 0.3125 and 22.4, which is not: it looked sharper on paper
        // and crawled in practice.
        match self {
            Self::High => ShadowPlan {
                resolution: 1024,
                distance: 128.0,
                taps: 3.0,
            },
            Self::Medium => ShadowPlan {
                resolution: 512,
                // 112 / 512 gives a 0.4375-block texel: 7 / 0.4375 = 16 exactly.
                // 96 gave 18.667 and crawled the same way High's 160 did.
                distance: 112.0,
                taps: 3.0,
            },
            Self::Low => ShadowPlan {
                resolution: 512,
                // 0.25-block texels: 28 per snap, as High's.
                distance: 64.0,
                taps: 3.0,
            },
        }
    }
    /// Whether `distance` at `resolution` keeps the snap on whole texels.
    ///
    /// `interval` is the pack's `shadowIntervalSize`; the origin moves in that
    /// many blocks, so it has to be a whole number of the map's texels or the
    /// map resamples itself on every redraw and the shadow crawls.
    pub fn snap_is_whole_texels(distance: f32, resolution: u32, interval: f32) -> bool {
        let texel = 2.0 * distance / resolution.max(1) as f32;
        let steps = interval / texel;
        (steps - steps.round()).abs() < 1e-4
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowPlan {
    pub resolution: u32,
    pub distance: f32,
    pub taps: f32,
}

/// Every visual knob, resolved once per frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub tier: Tier,
    pub on: bool,
    pub contrast: f32,
    pub saturation: f32,
    pub warmth: f32,
    pub bloom: f32,
    pub threshold: f32,
    pub radius: f32,
    pub vignette: f32,
    pub exposure: f32,
    pub shafts: f32,
    pub shadow: ShadowPlan,
}

impl Params {
    pub fn for_quality(quality: VisualQuality, on: bool) -> Self {
        let tier = Tier::from_quality(quality);
        // The pack's own look, from its `shader.h` defaults: a strong glow
        // (strength 1.0, threshold 0.45, halo 1.0) and light shafts at 2.0. The
        // grade stays neutral (no contrast/saturation/warmth of ours on top of
        // the pack's lighting), so the built-in profile is the pack's output.
        //
        // The glow is a chain of three textures the size of the output and two
        // passes, so it is on where the tier already pays for a scaled target
        // and off on Low, which renders at native size with no chain at all.
        // A tier that cannot afford it reads the pack's lighting only.
        let glow = !matches!(tier, Tier::Low);
        let (contrast, saturation, warmth) = (1.0, 1.0, 0.0);
        let (bloom, threshold, radius, vignette) = if glow {
            // BLOOM_STRENGTH, BLOOM_THRESHOLD, BLOOM_HALO_SIZE.
            (1.0, 0.45, 1.0, 0.0)
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };
        let exposure = 1.0;
        // SUN_SHAFTS_STRENGTH, at High only: they sample the sky at a quarter
        // of the pixels and the pass keys off the glow radius being High's.
        let shafts = if matches!(tier, Tier::High) { 2.0 } else { 0.0 };
        Self {
            tier,
            on,
            contrast,
            saturation,
            warmth,
            bloom,
            threshold,
            radius,
            vignette,
            exposure,
            shafts,
            shadow: tier.shadow(),
        }
    }

    pub fn uniform(self) -> [f32; 8] {
        [
            f32::from(self.on),
            self.contrast,
            self.saturation,
            self.warmth,
            self.bloom,
            self.threshold,
            self.radius,
            self.vignette,
        ]
    }

    pub fn post_uniform(self) -> [f32; 4] {
        [self.exposure, self.shafts, 0.0, 0.0]
    }

    /// The added GPU memory this tier's passes can hold, in bytes.
    pub fn bytes(self, output: [u32; 2]) -> u64 {
        let shadow = u64::from(self.shadow.resolution) * u64::from(self.shadow.resolution) * 4;
        let bloom = if self.bloom > 0.0 {
            let texels = u64::from(output[0].div_ceil(4)) * u64::from(output[1].div_ceil(4));
            texels * 8 * 3
        } else {
            0
        };
        shadow + bloom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_keep_the_pack_constants() {
        let high = Params::for_quality(VisualQuality::High, true);
        let medium = Params::for_quality(VisualQuality::Medium, true);
        let low = Params::for_quality(VisualQuality::Low, true);
        // The glow is the pack's own: strength 1.0, threshold 0.45, halo 1.0.
        for tier in [high, medium] {
            assert_eq!((tier.bloom, tier.threshold, tier.radius), (1.0, 0.45, 1.0));
            assert_eq!(tier.uniform(), [1.0, 1.0, 1.0, 0.0, 1.0, 0.45, 1.0, 0.0]);
        }
        // The grade stays neutral: the built-in look is the pack's lighting, not
        // ours on top of it.
        assert_eq!(
            (high.contrast, high.saturation, high.warmth),
            (1.0, 1.0, 0.0)
        );
        // Low allocates no chain at all, so it reads the pack's lighting only.
        assert_eq!((low.bloom, low.threshold, low.radius), (0.0, 0.0, 0.0));
        assert_eq!(low.uniform(), [1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        // SUN_SHAFTS_STRENGTH 2.0, at High only: they key off its glow radius.
        assert_eq!(high.shafts, 2.0);
        assert_eq!(medium.shafts, 0.0);
        assert_eq!(low.shafts, 0.0);
        // The tier without the glow allocates no chain: its extra cost is only
        // its shadow map, so the glow's three quarter-size textures show as the
        // difference beyond that.
        let output = [1920, 1080];
        let glow = u64::from(1920u32.div_ceil(4)) * u64::from(1080u32.div_ceil(4)) * 8 * 3;
        let shadow = |r: u32| u64::from(r) * u64::from(r) * 4;
        assert_eq!(
            high.bytes(output) - low.bytes(output),
            glow + shadow(1024) - shadow(512)
        );
        assert_eq!(high.post_uniform()[1], 2.0);
        assert_eq!(high.shadow.resolution, 1024);
        // High reaches 128 blocks of shadow on a 1024 map: 0.25-block texels.
        assert_eq!(high.shadow.distance, 128.0);
    }

    #[test]
    fn every_tier_keeps_its_snap_on_whole_texels() {
        // The shadow camera moves in `shadowIntervalSize` blocks (the pack's 7).
        // If that is not a whole number of the map's texels, every redraw
        // resamples the map and the shadow crawls sideways — placing a single
        // block was enough to see it.
        for tier in [Tier::High, Tier::Medium, Tier::Low] {
            let plan = tier.shadow();
            assert!(
                Tier::snap_is_whole_texels(plan.distance, plan.resolution, 7.0),
                "{}: {} blocks on a {} map is not a whole number of 7-block snaps",
                tier.label(),
                plan.distance,
                plan.resolution
            );
        }
        // The check itself must reject the values this bug came from.
        assert!(!Tier::snap_is_whole_texels(160.0, 1024, 7.0));
        assert!(!Tier::snap_is_whole_texels(96.0, 512, 7.0));
        assert!(Tier::snap_is_whole_texels(128.0, 1024, 7.0));
    }

    #[test]
    fn shadows_follow_the_pack_profiles() {
        let low = Params::for_quality(VisualQuality::Low, true);
        // A 512 map at 64 blocks rather than 256 at 64: half the texel size, so
        // the shadow a block casts is twice as sharp — the complaint was that
        // they were not accurate, not that they were too soft.
        assert_eq!(low.shadow.resolution, 512);
        assert_eq!(low.shadow.distance, 64.0);
        assert_eq!(low.bloom, 0.0);
        assert_eq!(low.shafts, 0.0);
        // Every tier filters. A single compare gives a stair-stepped edge a
        // block wide; no tier may ship that.
        for quality in [
            VisualQuality::High,
            VisualQuality::Medium,
            VisualQuality::Low,
        ] {
            let plan = Tier::from_quality(quality).shadow();
            assert_eq!(plan.taps, 3.0, "{quality:?} must filter its shadow edge");
            let texel = 2.0 * plan.distance / plan.resolution as f32;
            // A block's shadow must cover more than one texel, or it cannot be
            // drawn at all; 0.5 blocks a texel is the coarsest that reads.
            assert!(texel <= 0.5, "{quality:?}: {texel} blocks a texel");
        }
        let high = Tier::High.shadow();
        let medium = Tier::Medium.shadow();
        assert!(high.resolution >= medium.resolution);
        assert!(high.distance >= medium.distance);
    }

    #[test]
    fn a_pack_reach_keeps_the_density_the_map_was_built_for() {
        // MiniatureXL's `shader.h` defaults: 1024 texels over 128 blocks each side, so four texels
        // a block. The map cannot be resized once built, so applying the pack's 128-block reach to
        // a smaller map halves the density and the light mottles: 512 over 128 is 0.5 blocks a
        // texel against the pack's 0.25. Clamping to what the map covers keeps every lookup on a
        // texel the terrain pass wrote.
        let pack_density = 1024.0 / (2.0 * 128.0);
        for quality in [
            VisualQuality::High,
            VisualQuality::Medium,
            VisualQuality::Low,
        ] {
            let plan = Tier::from_quality(quality).shadow();
            // The renderer's own expression: `shadow_settings.distance` starts as the plan's and
            // is clamped to what the map covers at the pack's density.
            let covered = plan.resolution as f32 / (2.0 * pack_density);
            let reach = plan.distance.min(covered.min(128.0));
            let density = plan.resolution as f32 / (2.0 * reach);
            assert!(
                (density - pack_density).abs() < 1e-4,
                "{quality:?}: {density} texels a block against the pack's {pack_density}"
            );
            // Never widened past the pack's own reach.
            assert!(reach <= 128.0);
        }
    }

    #[test]
    fn the_default_memory_budget_fits() {
        for quality in [
            VisualQuality::High,
            VisualQuality::Medium,
            VisualQuality::Low,
        ] {
            let params = Params::for_quality(quality, true);
            assert!(
                params.bytes([3840, 2160]) <= 32 << 20,
                "{} bytes at 4K",
                params.bytes([3840, 2160])
            );
        }
    }
}
