use super::{atmosphere, grade};
use glam::Vec3;

/// Stable numeric samples checked without a GPU. These are Horizon's own
/// reference values, not extracted from another renderer.
pub fn atmosphere_samples() -> [[f32; 3]; 4] {
    [512.0, 4_096.0, 16_384.0, 65_536.0].map(|distance| {
        let mix = atmosphere::haze(distance, 0.0);
        [distance, mix, atmosphere::CAP]
    })
}

pub fn grade_samples() -> [Vec3; 3] {
    [
        grade::grade(Vec3::splat(0.18), 1.06, 1.14, 0.22, 1.02, 0.06, 0.0),
        grade::grade(Vec3::new(0.8, 0.55, 0.3), 1.06, 1.14, 0.22, 1.02, 0.06, 0.5),
        grade::grade(Vec3::new(1.8, 1.5, 0.9), 1.06, 1.14, 0.22, 1.02, 0.06, 1.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_deterministic_and_bounded() {
        let a = atmosphere_samples();
        assert_eq!(a, atmosphere_samples());
        for [distance, haze, cap] in a {
            assert!(distance > 0.0);
            assert!((0.0..=cap).contains(&haze));
            assert_eq!(cap, atmosphere::CAP);
        }
        assert_eq!(grade_samples(), grade_samples());
    }
}
