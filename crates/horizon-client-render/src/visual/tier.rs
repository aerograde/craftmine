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
        // One orthographic map over the near field. `taps` is the PCF width in
        // texels: a 3x3 tent on every tier, because a single compare gives a
        // shadow edge with hard stair-steps a block wide, which is what "the
        // shadows look wrong" was. Nine taps once per terrain pixel is small
        // beside the terrain shader itself, and the cheaper tiers already pay
        // for it with a smaller map rather than with a jagged edge.
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
        // The pack has no post grade, bloom or vignette: its look comes from
        // terrain lighting, torch colour, fog and the sky. Keep the grade
        // neutral so the port is the pack's output, not ours on top of it.
        let (contrast, saturation, warmth, bloom, threshold, radius, vignette) =
            (1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let exposure = 1.0;
        let shafts = 0.0;
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
        assert_eq!(high.uniform(), [1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(medium.uniform(), high.uniform());
        assert_eq!(low.uniform(), high.uniform());
        assert_eq!(high.post_uniform(), [1.0, 0.0, 0.0, 0.0]);
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
