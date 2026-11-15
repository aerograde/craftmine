// Horizon's world post pass: the world, rendered at the resolution scale,
// copied (bilinear) or anti-aliased with FXAA onto the screen before the
// hand and HUD. Only used when a non-vanilla setting asks for it.
//
// The Miniature Shader profile lives here: a quarter-resolution bright pass
// (threshold, light shafts), a separable Gaussian bloom, and one composite
// that applies exposure, colour grade, the highlight shoulder and vignette.
// Running the glow at a quarter of the pixels costs a fraction of the old
// full-resolution taps while spreading it much wider, which is what the
// profile's soft, filmic highlights need.
diagnostic(off, derivative_uniformity);
@group(0) @binding(0) var world: texture_2d<f32>;
@group(0) @binding(1) var world_sampler: sampler;
// The depth of the world target: the bright pass reads it (unused today, kept
// for graded depth effects), TAA reprojects through it.
@group(0) @binding(2) var depth: texture_depth_2d;
// The blurred bright pass, at a quarter of the world's size.
@group(0) @binding(3) var bloom_source: texture_2d<f32>;
@group(0) @binding(4) var<uniform> taa: Taa;
// TAA's previous frame, at the output size (upscaling makes it larger).
@group(0) @binding(6) var history: texture_2d<f32>;
// The blur pass's step: xy the texel offset of this direction.
@group(0) @binding(7) var<uniform> blur_direction: vec4<f32>;

struct Visual {
    // x: enabled, y: contrast, z: saturation, w: warmth.
    params: vec4<f32>,
    // x: bloom strength, y: bloom threshold, z: bloom radius in texels,
    // w: vignette.
    extra: vec4<f32>,
    // x: daylight (0 night, 1 noon), y: dawn/dusk, z/w: the sun's screen position.
    mood: vec4<f32>,
    // x: exposure, y: spare, z: spare, w: spare.
    post: vec4<f32>,
};
@group(0) @binding(5) var<uniform> visual: Visual;

fn sample_world(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(world, world_sampler, uv, 0.0).rgb;
}

// The bright pass's contribution of one channel triple above `threshold`, with
// a soft knee so that a bright surface does not switch the glow on at a step.
// The threshold reads luminance, not the brightest channel: a clear noon sky
// has a full blue channel but is not a highlight, while the sun, cloud tops
// and glowing blocks are.
fn bright_part(c: vec3<f32>) -> vec3<f32> {
    let threshold = max(visual.extra.y, 0.01);
    let l = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let weight = smoothstep(threshold, threshold + 0.18, l);
    return max(c - vec3<f32>(threshold), vec3<f32>(0.0)) * weight;
}

// The sun's light through the canopy: bright pixels pulled toward the sun,
// sampled at a quarter of the pixels like the rest of the glow. High only
// (`extra.z`, the bloom radius, is above Medium's), and only while the sun is
// low and on screen.
fn light_shafts(uv: vec2<f32>) -> vec3<f32> {
    if visual.params.x < 0.5 || visual.extra.x <= 0.0 || visual.post.y < 0.5 {
        return vec3<f32>(0.0);
    }
    let dusk = clamp(visual.mood.y, 0.0, 1.0);
    if dusk < 0.05 {
        return vec3<f32>(0.0);
    }
    let sun = visual.mood.zw;
    if any(sun < vec2<f32>(-0.25)) || any(sun > vec2<f32>(1.25)) {
        return vec3<f32>(0.0);
    }
    let delta = (sun - uv) / 6.0;
    var p = uv;
    var acc = vec3<f32>(0.0);
    var weight = 1.0;
    for (var i = 0; i < 6; i = i + 1) {
        p = p + delta;
        let c = sample_world(p);
        let bright = max(dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)) - 0.68, 0.0);
        acc = acc + c * (bright * weight);
        weight = weight * 0.82;
    }
    return acc * (0.05 * dusk);
}

struct PostOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex fn vs_post(@builtin(vertex_index) i: u32) -> PostOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: PostOut;
    out.clip = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(p.x, 1.0 - p.y);
    return out;
}

// The world's picture after the profile: exposure, saturation, a filmic
// contrast curve, cool shade against warm light, a soft highlight shoulder
// and a round vignette. `c` already carries the bloom.
fn grade(c: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    if visual.params.x < 0.5 {
        return c;
    }
    let dusk = clamp(visual.mood.y, 0.0, 1.0);
    // Exposure, a touch higher in the golden hour, brings the frame into the
    // filmic curve; below 1.0 keeps a bright noon from clipping.
    var graded = max(c, vec3<f32>(0.0)) * (visual.post.x * mix(1.0, 1.05, dusk));
    // Miniature Shader's vivid daylight: saturated, but the shade keeps its
    // own colour instead of turning grey.
    let l = dot(graded, vec3<f32>(0.2126, 0.7152, 0.0722));
    graded = mix(vec3<f32>(l), graded, visual.params.z * (1.0 - 0.12 * dusk));
    // A filmic S-curve: crisp by day, gentler in the haze of dawn and dusk.
    let contrast = mix(visual.params.y, 1.0 + (visual.params.y - 1.0) * 0.55, dusk);
    graded = (graded - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5);
    // Cool daylight shadows against warm light: blue-grey shade under a pale
    // sky, easing to neutral as the sun sets.
    graded = graded
        + vec3<f32>(0.006, 0.008, 0.016) * (1.0 - l) * (1.0 - dusk)
        + vec3<f32>(0.016, 0.008, 0.002) * (1.0 - l) * dusk;
    // Warmth: a touch at noon so greens stay lively, amber at golden hour.
    let warmth = visual.params.w * (1.0 + 1.1 * dusk);
    graded = graded * vec3<f32>(1.0 + 0.055 * warmth, 1.0 + 0.006 * warmth, 1.0 - 0.045 * warmth);
    // Keep highlights pastel instead of clipping hard (the sun, cloud tops,
    // sandstone, white concrete).
    let over = max(graded - vec3<f32>(0.78), vec3<f32>(0.0));
    graded = graded - over * (0.30 + 0.10 * dusk);
    // A round vignette, so the corners of a wide frame darken evenly. Scale by
    // the corner distance: multiplying by the aspect ratio darkened the sides
    // three times as much as the top and bottom (and more on wider screens).
    let size = max(vec2<f32>(textureDimensions(world)), vec2<f32>(1.0));
    let aspect = size.x / size.y;
    let corner = 0.5 * length(vec2<f32>(aspect, 1.0));
    let d = length((uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0)) / corner;
    graded = graded * (1.0 - visual.extra.w * smoothstep(0.38, 0.92, d));
    return max(graded, vec3<f32>(0.0));
}

// The world plus the glow, then the profile.
fn compose(c: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    if visual.params.x < 0.5 {
        return c;
    }
    // Low drops the glow chain; do not fetch its (blank) texture every pixel.
    var lit = c;
    if visual.extra.x > 0.0 {
        let bloom = textureSampleLevel(bloom_source, world_sampler, uv, 0.0).rgb;
        // The Gaussian keeps its energy, so a strength scaled for the old ring of
        // full-resolution taps reads the same here, spread wider.
        lit += bloom * (visual.extra.x * 1.8);
    }
    return grade(lit, uv);
}

@fragment fn fs_copy(in: PostOut) -> @location(0) vec4<f32> {
    // The alpha carries the emissive mark through to HDR output (hdr.wgsl).
    let c = textureSampleLevel(world, world_sampler, in.uv, 0.0);
    return vec4<f32>(compose(c.rgb, in.uv), c.a);
}

// TAA's history is ungraded light (so its neighbourhood clamp compares like
// with like): the profile is applied here, once per output pixel.
@fragment fn fs_present(in: PostOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(world, world_sampler, in.uv, 0.0);
    return vec4<f32>(compose(c.rgb, in.uv), c.a);
}

// The bright pass, at a quarter of the world's pixels: everything above the
// threshold, softened, plus the golden-hour shafts. Four bilinear taps cover
// the quarter pixel's whole footprint, so a bright glint between the taps is
// not missed (a single tap would flicker as the camera moved).
fn bright_source(uv: vec2<f32>) -> vec3<f32> {
    // The quarter-res target's footprint is 4x4 world texels. Offsets of 1.5
    // put the four taps in the four 2x2 quadrants: the old 0.5 offsets covered
    // only the central 2x2, so a one-texel glint could pop in and out of the
    // glow as the camera moved.
    let texel = 1.5 / max(vec2<f32>(textureDimensions(world)), vec2<f32>(1.0));
    return 0.25 * (sample_world(uv + vec2<f32>(texel.x, texel.y))
        + sample_world(uv + vec2<f32>(-texel.x, texel.y))
        + sample_world(uv + vec2<f32>(texel.x, -texel.y))
        + sample_world(uv + vec2<f32>(-texel.x, -texel.y)));
}

@fragment fn fs_bloom_bright(in: PostOut) -> @location(0) vec4<f32> {
    if visual.params.x < 0.5 || visual.extra.x <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let c = bright_source(in.uv);
    return vec4<f32>(bright_part(c) + light_shafts(in.uv), 1.0);
}

// One direction of the separable Gaussian (9 taps then, at a quarter of the
// pixels, a fraction of the old full-resolution ring's cost).
@fragment fn fs_bloom_blur(in: PostOut) -> @location(0) vec4<f32> {
    let texel = 1.0 / max(vec2<f32>(textureDimensions(world)), vec2<f32>(1.0));
    let offsets = array<f32, 4>(1.0, 2.0, 3.0, 4.0);
    let weights = array<f32, 4>(0.1945946, 0.1216216, 0.0540541, 0.0162162);
    // The profile's radius was never read: High's 10 is the 1.5 here, and
    // Medium's 8 narrows the glow instead of paying for High's width.
    let spread = max(visual.extra.z, 1.0) * 0.15;
    var sum = sample_world(in.uv) * 0.2270270;
    for (var i = 0; i < 4; i = i + 1) {
        let o = blur_direction.xy * texel * offsets[i] * spread;
        sum = sum + sample_world(in.uv + o) * weights[i];
        sum = sum + sample_world(in.uv - o) * weights[i];
    }
    return vec4<f32>(sum, 1.0);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.299, 0.587, 0.114));
}

// FXAA 3.11 (console quality): blend along the local edge direction.
fn fxaa_color(in: PostOut) -> vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(world));
    let centre = textureSampleLevel(world, world_sampler, in.uv, 0.0);
    let rgb_m = centre.rgb;
    let nw = luma(textureSampleLevel(world, world_sampler, in.uv + vec2(-0.5, -0.5) * texel, 0.0).rgb);
    let ne = luma(textureSampleLevel(world, world_sampler, in.uv + vec2(0.5, -0.5) * texel, 0.0).rgb);
    let sw = luma(textureSampleLevel(world, world_sampler, in.uv + vec2(-0.5, 0.5) * texel, 0.0).rgb);
    let se = luma(textureSampleLevel(world, world_sampler, in.uv + vec2(0.5, 0.5) * texel, 0.0).rgb);
    let m = luma(rgb_m);
    let lo = min(m, min(min(nw, ne), min(sw, se)));
    let hi = max(m, max(max(nw, ne), max(sw, se)));
    if hi - lo < max(0.0312, hi * 0.125) {
        return centre;
    }
    var dir = vec2(-((nw + ne) - (sw + se)), (nw + sw) - (ne + se));
    let reduce = max((nw + ne + sw + se) * 0.25 * 0.125, 1.0 / 128.0);
    let scale = 1.0 / (min(abs(dir.x), abs(dir.y)) + reduce);
    dir = clamp(dir * scale, vec2(-8.0), vec2(8.0)) * texel;
    let a = 0.5 * (textureSampleLevel(world, world_sampler, in.uv + dir * (1.0 / 3.0 - 0.5), 0.0).rgb
        + textureSampleLevel(world, world_sampler, in.uv + dir * (2.0 / 3.0 - 0.5), 0.0).rgb);
    let b = a * 0.5 + 0.25 * (textureSampleLevel(world, world_sampler, in.uv - dir * 0.5, 0.0).rgb
        + textureSampleLevel(world, world_sampler, in.uv + dir * 0.5, 0.0).rgb);
    let lb = luma(b);
    if lb < lo || lb > hi {
        return vec4(a, centre.a);
    }
    return vec4(b, centre.a);
}

@fragment fn fs_fxaa(in: PostOut) -> @location(0) vec4<f32> {
    let c = fxaa_color(in);
    return vec4<f32>(compose(c.rgb, in.uv), c.a);
}

// TAA (`world_post.rs`): blend this frame into the history, reprojected from the
// depth buffer through the camera's motion and clamped to this frame's 3x3
// neighbourhood so that what moved or appeared does not ghost.
struct Taa {
    reproject: mat4x4<f32>,
    // xy: this frame's jitter in clip space; z: 1 when the history is unusable.
    jitter: vec4<f32>,
};

// Catmull-Rom over the history in 5 bilinear taps: sharper than one under motion.
fn sample_history(uv: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(history));
    let pos = uv * size;
    let centre = floor(pos - 0.5) + 0.5;
    let f = pos - centre;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let t0 = (centre - 1.0) / size;
    let t3 = (centre + 2.0) / size;
    let t12 = (centre + w2 / w12) / size;
    let c = textureSampleLevel(history, world_sampler, vec2(t12.x, t0.y), 0.0).rgb * (w12.x * w0.y)
        + textureSampleLevel(history, world_sampler, vec2(t0.x, t12.y), 0.0).rgb * (w0.x * w12.y)
        + textureSampleLevel(history, world_sampler, t12, 0.0).rgb * (w12.x * w12.y)
        + textureSampleLevel(history, world_sampler, vec2(t3.x, t12.y), 0.0).rgb * (w3.x * w12.y)
        + textureSampleLevel(history, world_sampler, vec2(t12.x, t3.y), 0.0).rgb * (w12.x * w3.y);
    let weight = w12.x * w0.y + w0.x * w12.y + w12.x * w12.y + w3.x * w12.y + w12.x * w3.y;
    return max(c / weight, vec3(0.0));
}

// It runs at the history's size, which upscaling makes larger than the world's.
// The history is unjittered: each output pixel takes the world pixel whose
// jittered sample lands nearest its centre, weighted by how near it lands.
@fragment fn fs_taa(in: PostOut) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(world));
    // The jitter moved the scene by `jitter` in clip space: where this pixel's
    // point was drawn, in world pixels.
    let drawn = (in.uv + taa.jitter.xy * vec2(0.5, -0.5)) * size;
    let last = vec2<i32>(size) - 1;
    let p = clamp(vec2<i32>(floor(drawn)), vec2(0), last);
    let centre = textureLoad(world, p, 0);
    var lo = centre.rgb;
    var hi = centre.rgb;
    // Reversed depth: the nearest of the 3x3 is the largest, and its motion keeps
    // the edges of what is in front from trailing.
    var near = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let q = clamp(p + vec2(x, y), vec2(0), last);
            let c = textureLoad(world, q, 0).rgb;
            lo = min(lo, c);
            hi = max(hi, c);
            near = max(near, textureLoad(depth, q, 0));
        }
    }
    let ndc = vec2(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let previous = taa.reproject * vec4(ndc, near, 1.0);
    let uv = vec2(0.5, -0.5) * previous.xy / previous.w + 0.5;
    // The history holds raw world light, like this frame's target: the profile
    // runs later in `fs_present`. Grading here made the clamp compare a graded
    // history with ungraded bounds, which pulled almost every pixel back to
    // the ungraded colour under TAA and upscaling.
    let current = centre.rgb;
    if taa.jitter.z > 0.0 || any(uv != clamp(uv, vec2(0.0), vec2(1.0))) {
        return vec4(current, centre.a);
    }
    let past = clamp(sample_history(uv), lo, hi);
    // How far the sample landed from this pixel's centre, in output pixels: a
    // Gaussian of it scales the new frame's share, so that frames whose sample
    // fell far away (most of them when upscaling) barely move the pixel.
    let off = (drawn - (vec2<f32>(p) + 0.5)) * vec2<f32>(textureDimensions(history)) / size;
    let share = 0.1 * exp(-2.0 * dot(off, off));
    // The alpha carries the emissive mark through to HDR output (hdr.wgsl).
    return vec4(mix(past, current, share), centre.a);
}
