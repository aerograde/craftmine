use glam::Vec3;

/// The CPU reference of the post profile's grade, used to validate the WGSL
/// constants without a GPU.
pub fn grade(
    color: Vec3,
    contrast: f32,
    saturation: f32,
    warmth: f32,
    exposure: f32,
    vignette: f32,
    distance: f32,
) -> Vec3 {
    let mut graded = color.max(Vec3::ZERO) * exposure;
    let luma = graded.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    graded = Vec3::splat(luma).lerp(graded, saturation);
    graded = (graded - Vec3::splat(0.5)) * contrast + Vec3::splat(0.5);
    graded += Vec3::new(0.006, 0.008, 0.016) * (1.0 - luma);
    graded *= Vec3::new(
        1.0 + 0.055 * warmth,
        1.0 + 0.006 * warmth,
        1.0 - 0.045 * warmth,
    );
    let over = (graded - Vec3::splat(0.78)).max(Vec3::ZERO);
    graded -= over * 0.30;
    graded *= 1.0 - vignette * smoothstep(0.38, 0.92, distance);
    graded.max(Vec3::ZERO)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturation_preserves_luma_at_neutral_contrast() {
        let c = Vec3::new(0.4, 0.5, 0.3);
        let luma = Vec3::new(0.2126, 0.7152, 0.0722);
        // The profile lifts the shade toward blue and warms the light, so luma
        // moves on its own between saturations. Compare saturation 1 and 2 to
        // isolate what the saturation term does.
        let neutral = grade(c, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0);
        let saturated = grade(c, 1.0, 2.0, 0.0, 1.0, 0.0, 0.0);
        assert!((neutral.dot(luma) - saturated.dot(luma)).abs() < 1.0e-5);
    }

    #[test]
    fn output_never_goes_negative() {
        for x in [-2.0, -0.1, 0.0, 0.4, 1.0, 3.0] {
            let c = Vec3::splat(x);
            assert!(
                grade(c, 1.1, 1.2, 0.3, 1.02, 0.08, 1.0)
                    .cmpge(Vec3::ZERO)
                    .all()
            );
        }
    }

    #[test]
    fn the_shoulder_rolls_highlights_off() {
        let bright = Vec3::splat(2.0);
        let graded = grade(bright, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0);
        assert!(graded.x < bright.x);
    }
}
