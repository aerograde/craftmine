//! A 1:1 port of Minecraft Miniature Shader's lighting and atmosphere model.
//!
//! Adapted from Minecraft Miniature Shader, Copyright (c) 2022 Mateus A. Kreuch,
//! MIT License. See THIRD_PARTY_NOTICES.md.
//!
//! This is the CPU reference for the values and curves `terrain.wgsl` runs on the
//! GPU. It keeps the pack's constants, including its default profile (NORMAL).

use glam::{Vec3, Vec3Swizzles};

pub const SHADOW_DARKNESS: f32 = 0.0;
pub const LIGHT_BRIGHTNESS: f32 = 0.9;
pub const SHADOW_BLUENESS: f32 = 0.2;

pub const TORCH_INNER: [f32; 3] = [1.0, 0.85, 0.7];
pub const TORCH_MIDDLE: [f32; 3] = [1.0, 0.8, 0.6];
pub const TORCH_OUTER: [f32; 3] = [1.0, 0.55, 0.2];
pub const MOON: [f32; 3] = [0.1, 0.15, 0.3];

/// The lightmap texel the pack reads for ambient light (block 0, sky 1).
pub const AMBIENT_BLOCK: f32 = 8.0 / 255.0;
/// The block-light range where torch colour ramps from none to full.
pub const TORCH_UV: (f32, f32) = (8.0 / 255.0, 231.0 / 255.0);

pub const OVERWORLD_FOG_MIN: f32 = 1.0 - 0.1 * 7.0;
pub const OVERWORLD_FOG_MAX: f32 = 1.0;

pub const NOON: f32 = 0.25;
pub const SUNSET: f32 = 0.5325;
pub const MIDNIGHT: f32 = 0.75;
pub const SUNRISE: f32 = 0.9675;

pub const SHADOW_COLOR: Vec3 = Vec3::new(
    1.0 - SHADOW_DARKNESS,
    1.0 - SHADOW_DARKNESS + 0.3333 * SHADOW_BLUENESS,
    1.0 - SHADOW_DARKNESS + SHADOW_BLUENESS,
);

pub fn luma(color: Vec3) -> f32 {
    color.dot(Vec3::new(0.299, 0.587, 0.114))
}

pub fn rescale(x: f32, a: f32, b: f32) -> f32 {
    ((x - a) / (b - a)).clamp(0.0, 1.0)
}

pub fn smoothe(x: f32) -> f32 {
    x * x * (3.0 - 2.0 * x)
}

pub fn slope_to_1(x: f32, m: f32) -> f32 {
    (x * m - (m - 1.0)).max(0.0)
}

pub fn fogify(x: f32, w: f32) -> f32 {
    w / (x * x + w)
}

/// The ambient lightmap colour the pack builds from the sky texel.
pub fn ambient(lightmap: Vec3, sun_height: f32, screen_brightness: f32) -> Vec3 {
    let x = lightmap.y;
    let mut final_value = (((0.8494 * x + 0.9687) * x - 5.238) * x + 3.711) * x - 0.2864;
    final_value = final_value.max(0.0);
    let boosted =
        Vec3::ONE.min(lightmap + Vec3::splat(final_value) * (0.2 * sun_height).clamp(0.0, 1.0));
    boosted.lerp(lightmap, screen_brightness.clamp(0.0, 1.0))
}

/// The pack's torch contribution: a warm ramp over the block-light level.
pub fn torch(block_light: f32, ambient: Vec3, screen_brightness: f32, eye_sky: f32) -> Vec3 {
    let mut strength = rescale(block_light, TORCH_UV.0, TORCH_UV.1);
    strength =
        strength * strength * (1.0 - screen_brightness) + smoothe(strength) * screen_brightness;
    let blend = (1.0 - screen_brightness)
        .max(eye_sky / 240.0)
        .clamp(0.0, 1.0);
    strength = strength * strength * (1.0 - blend) + strength * blend;
    (1.0 - luma(ambient)).max(0.0)
        * strength
        * Vec3::from(TORCH_OUTER)
            .lerp(Vec3::from(TORCH_MIDDLE), strength)
            .lerp(Vec3::from(TORCH_INNER), slope_to_1(strength, 8.0))
}

pub fn light_color(sun_height: f32, sky_light: f32) -> Vec3 {
    let sun_redness = 1.0 - (0.2 * sun_height - 3.929).clamp(0.0, 1.0);
    let color = if sun_height > 0.01 {
        Vec3::new(1.0 + sun_redness.clamp(0.12, 1.0), 1.06, 1.0).normalize()
    } else {
        Vec3::from(MOON)
    };
    Vec3::splat(luma(color)).lerp(color, sky_light.clamp(0.0, 1.0))
}

/// `getDiffuse`: the sun's strength on a face, as the pack scales it.
pub fn diffuse(
    sun_height: f32,
    sky_light: f32,
    normal: Vec3,
    light: Vec3,
    fog_mix: f32,
    rain: f32,
    underwater: bool,
) -> f32 {
    let surface = if underwater { 0.5 } else { 1.0 };
    surface
        * (1.0 - fog_mix)
        * (1.0 - rain)
        * (2.5 * normal.normalize().dot(light.normalize())).clamp(-0.3333, 1.0)
        * (0.1 * sun_height.abs() - 0.4453).clamp(0.0, 1.0)
        * rescale(sky_light, 0.3137, 0.6235)
}

/// `getFogMix`: the pack's distance fog, including its day/night start multiplier.
pub fn fog_mix(pos: Vec3, start: f32, end: f32, far: f32, time: f32, rain: f32) -> f32 {
    let x = (25.0
        * if time < MIDNIGHT {
            SUNSET - time
        } else {
            time - SUNRISE
        }
        + 0.3)
        .clamp(OVERWORLD_FOG_MIN, OVERWORLD_FOG_MAX);
    let x = x.min(1.0 - rain);
    let length = pos.length();
    let cylinder = (pos.x * pos.x + pos.z * pos.z).sqrt().max(pos.y.abs());
    let start_mult = x * start;
    let cylinder_start = x * (far - (0.1 * far).clamp(4.0, 64.0));
    rescale(length, start_mult, end).max(rescale(cylinder, cylinder_start, far))
}

pub fn gradient_fog_color(fog: Vec3, sky: Vec3, fog_mix: f32, feet_y: f32) -> Vec3 {
    fog.lerp(sky, fog_mix * (0.006 * feet_y).clamp(0.0, 1.0))
}

/// `getWaterWave`: the surface normal's ripple, scaled by distance.
pub fn water_wave(random: f32, feet: Vec3, time: f32, size: f32, speed: f32) -> Vec3 {
    if size <= 0.4 {
        return Vec3::ZERO;
    }
    let mut v = (20.0 / (size - 0.4)) / feet.xz().length().max(1.0);
    v = v.min(1.0);
    Vec3::new(
        (random * speed * time).sin().powi(3),
        0.0,
        (random * speed * time).cos().powi(3),
    ) * v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pack_constants_are_preserved() {
        assert_eq!(LIGHT_BRIGHTNESS, 0.9);
        assert!((SHADOW_COLOR - Vec3::new(1.0, 1.06666, 1.2)).length() < 1.0e-5);
        assert_eq!(TORCH_INNER, [1.0, 0.85, 0.7]);
        assert_eq!(MOON, [0.1, 0.15, 0.3]);
        assert_eq!(OVERWORLD_FOG_MIN, 0.3);
    }

    #[test]
    fn torch_is_warm_and_falls_off_with_light() {
        let none = torch(0.0, Vec3::ZERO, 0.0, 0.0);
        let full = torch(1.0, Vec3::ZERO, 0.0, 0.0);
        assert_eq!(none, Vec3::ZERO);
        assert!(full.x > full.y && full.y > full.z);
    }

    #[test]
    fn daylight_is_warmer_than_moonlight() {
        let day = light_color(0.5, 1.0);
        let night = light_color(-0.5, 1.0);
        assert!(day.x > day.z);
        assert!(night.z > night.x);
    }

    #[test]
    fn fog_reaches_full_at_the_end() {
        let near = fog_mix(Vec3::new(0.0, 64.0, 0.0), 64.0, 128.0, 256.0, NOON, 0.0);
        let far = fog_mix(Vec3::new(0.0, 64.0, 128.0), 64.0, 128.0, 256.0, NOON, 0.0);
        assert_eq!(near, 0.0);
        assert!(far > 0.9);
    }

    #[test]
    fn the_shader_runs_the_same_model() {
        let terrain = include_str!("../terrain.wgsl");
        assert!(terrain.contains("MINIATURE_LIGHT_BRIGHTNESS: f32 = 0.9"));
        assert!(terrain.contains("miniature_lit"));
        assert!(terrain.contains("miniature_fog_mix"));
        assert!(terrain.contains("miniature_fogify"));
    }
}
