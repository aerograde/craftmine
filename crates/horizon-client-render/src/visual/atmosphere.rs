use glam::Vec3;

pub const START: f32 = 512.0;
pub const DAY_DENSITY: f32 = 9_000.0;
pub const DUSK_DENSITY: f32 = 4_200.0;
pub const CAP: f32 = 0.72;

/// The aerial-perspective amount at `distance`; a depth cue, never a mask.
pub fn haze(distance: f32, dusk: f32) -> f32 {
    if distance <= START {
        return 0.0;
    }
    let dusk = dusk.clamp(0.0, 1.0);
    let density = DAY_DENSITY + (DUSK_DENSITY - DAY_DENSITY) * dusk;
    (1.0 - (-(distance - START) / density).exp()) * CAP
}

pub fn color(base: Vec3, sunrise: Vec3, dusk: f32, toward_sun: f32) -> Vec3 {
    let dusk = dusk.clamp(0.0, 1.0);
    let toward = toward_sun.clamp(0.0, 1.0);
    base.lerp(sunrise, 0.55 * dusk * (0.5 + 0.5 * toward * toward))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haze_stays_a_depth_cue() {
        assert_eq!(haze(0.0, 0.0), 0.0);
        assert_eq!(haze(START, 0.0), 0.0);
        let near = haze(4_000.0, 0.0);
        let far = haze(65_536.0, 0.0);
        assert!(near > 0.0 && near < far);
        assert!(far <= CAP + 1e-6);
        assert!(far < 1.0, "the horizon must stay readable");
    }

    #[test]
    fn the_shader_reads_the_same_haze_curve() {
        let terrain = include_str!("../terrain.wgsl");
        assert!(terrain.contains("if d < 512.0"));
        assert!(terrain.contains("mix(9000.0, 4200.0, dusk)"));
        assert!(terrain.contains("* 0.72"));
        assert!(terrain.contains("smoothstep(57344.0, 65536.0"));
    }

    #[test]
    fn haze_is_monotone_with_distance() {
        let mut previous = 0.0;
        for distance in (512..=65_536).step_by(512) {
            let value = haze(distance as f32, 0.3);
            assert!(value >= previous);
            previous = value;
        }
    }
}
