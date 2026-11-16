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
        // texels: 3 on High because nine compares of a 1024² map is cheap next
        // to the terrain shader, 0 on the integrated tiers where the extra
        // samples cost more than the edge softness is worth there.
        match self {
            Self::High => ShadowPlan {
                resolution: 1024,
                distance: 128.0,
                taps: 3.0,
            },
            Self::Medium => ShadowPlan {
                resolution: 512,
                distance: 64.0,
                taps: 0.0,
            },
            Self::Low => ShadowPlan {
                resolution: 256,
                distance: 64.0,
                taps: 0.0,
            },
        }
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
        assert_eq!(high.shadow.distance, 128.0);
    }

    #[test]
    fn shadows_follow_the_pack_profiles() {
        let low = Params::for_quality(VisualQuality::Low, true);
        assert_eq!(low.shadow.resolution, 256);
        assert_eq!(low.shadow.distance, 64.0);
        assert_eq!(low.bloom, 0.0);
        assert_eq!(low.shafts, 0.0);
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
