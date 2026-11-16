// Shared LOD shading for the Voxy patches (voxy_opaque / voxy_translucent).
//
// Contract, read from Voxy 0.2.20 and Iris 1.11 source:
// - Iris preprocesses this file like any program (#include, options, MC_VERSION,
//   VOXY=3), then Voxy appends the result to ITS OWN fragment shader, a
//   `#version 460 core` program. So: no #version, no #extension, no
//   texture2D, no varyings, no derivatives, no GLSL 120 built-ins.
// - Declare NO uniform and NO sampler here. Voxy injects every name listed in
//   voxy.json "uniforms" as a member of a uniform block, and every name in
//   "samplers" as a bound sampler2D; a second declaration fails to compile.
//   Shared includes keep their own `uniform` lines inside #ifndef VOXY_PATCH.
// - `lightmap` is the vanilla level lightmap texture (Voxy maps that sampler
//   name to the game's lightmap), so the LOD ambient is the very same lookup
//   as gbuffers' getAmbientColor - no approximation.
// - parameters.lightMap is ALREADY a lightmap UV, (16*level + 8) / 256 per
//   axis (x = block light, y = sky light), exactly what gbuffers get from
//   gl_TextureMatrix[1] * gl_MultiTexCoord1.
// - Voxy rewrites gl_FragCoord.z to forward Z before compiling, and vxProjInv
//   is the matching forward-Z inverse projection.
//
// Pieces that only exist per vertex in gbuffers (diffuse from gl_Normal, the
// torch gradient without the voxel field) are mirrored below: KEEP IN SYNC.

#define VOXY_PATCH
#ifndef VOXY
#define VOXY
#endif

#include "/shader.h"

// core profile: texture2D is gone, getAmbientColor uses it on the lightmap
#define texture2D texture

#include "/common/math.glsl"
#include "/common/getFogMix.vsh"
#include "/common/getFogColor.vsh"
#include "/common/getAmbientColor.glsl"
#include "/common/getLightColor.vsh"
#include "/common/lightPalette.glsl"
#include "/common/lightContrast.glsl"

// LOD fragment position relative to the player (gbuffers' feetPos), through
// the exact matrices Voxy projected it with
vec3 voxyFeetPos() {
   vec3 ndc = vec3(gl_FragCoord.xy / vec2(viewWidth, viewHeight), gl_FragCoord.z) * 2.0 - 1.0;
   vec4 view = vxProjInv * vec4(ndc, 1.0);
   return (vxModelViewInv * vec4(view.xyz / view.w, 1.0)).xyz;
}

float voxySunHeight() {
   return (gbufferModelViewInverse * vec4(sunPosition, 1.0)).y;
}

// Voxy's face index -> world-space axis normal (expression from Voxy's author)
vec3 voxyWorldNormal(uint face) {
   return vec3(uint((face >> 1) == 2u), uint((face >> 1) == 0u), uint((face >> 1) == 1u))
        * (float(int(face) & 1) * 2.0 - 1.0);
}

// gbuffers get the emitted light level per vertex (at_midBlock.w / 15). LODs
// only have the Iris block id: nominal vanilla levels per block.properties
// group. groups whose emission depends on block state (redstone torch/lamp,
// candles, furnaces, lit redstone ore) count as dark
float voxyLightSourceLevel(float id) {
   if (id == 10068.0 || id == 10072.0) return 1.0;            // lava, fire
   if (id == 10201.0 || id == 10205.0 || id == 10207.0 || id == 10257.0) return 1.0; // torch/lantern, sea lantern, glowstone, jack o'lantern
   if (id >= 10215.0 && id <= 10218.0) return 1.0;            // froglights, copper torch/lantern
   if (id == 10213.0) return 14.0/15.0;                        // end rod
   if (id == 10202.0 || id == 10206.0 || id == 10260.0) return 10.0/15.0; // soul lights, portal / crying obsidian, soul fire
   if (id == 10214.0 || id == 10211.0) return 7.0/15.0;       // glow lichen, ender chest / end portal frame
   if (id == 10204.0) return 0.33;                             // amethyst buds/cluster (floor)
   if (id == 10208.0) return 3.0/15.0;                         // magma
   if (id == 10212.0) return 0.27;                             // brewing stand (floor)
   if (id == 10219.0 || id == 10221.0 || id == 10222.0) return 0.2; // sculk, redstone (floor)
   if (id >= 10264.0 && id <= 10266.0) return 0.2;            // calibrated sculk sensors (floor)
   if (id == 10262.0) return 7.0/15.0;                         // enchanting table
   if (id == 10263.0) return 1.0;                              // beacon
   if (id == 10223.0 || id == 10224.0) return 0.13;            // nether stems (floor)
   if (id == 10225.0) return 0.33;                             // awake creaking heart
   if (id == 10226.0) return 0.2;                              // open eyeblossom
   if (id == 10227.0) return 14.0/15.0;                        // glow berries
   if (id == 10228.0) return 0.27;                             // nether fungi, torchflower
   if (id == 10248.0) return 0.17;                             // spawner
   return 0.0;
}

// KEEP IN SYNC with common/getTorchColor.fsh. At LOD distance the hand light
// (reach <= 15 blocks), the voxel field (fades out over CL_RANGE_EFF) and the
// held-item hue are all zero, leaving the base gradient and the saturation step
vec3 voxyTorchColor(float torchLight, vec3 ambient) {
   float strength = rescale(torchLight, TORCH_UV_SCALE.x, TORCH_UV_SCALE.y);

   strength = mix(strength*strength, smoothe(strength), screenBrightness);
   strength = mix(strength*strength, strength, max(1.0 - screenBrightness, eyeBrightnessSmooth.y/240.0));
   strength = pow(strength, 1.0 / TORCH_FALLOFF);
   strength = contrastTorchStrength(strength);

   vec3 torchColor = mix(
      mix(TORCH_OUTER_COLOR, TORCH_MIDDLE_COLOR, strength),
      TORCH_INNER_COLOR,
      slopeTo1(strength, 8.0)
   );

   #ifdef COLORED_LIGHT
      float torchPeak = max(torchColor.r, max(torchColor.g, torchColor.b));

      torchColor = max(vec3(0.0), mix(
         vec3(luma(torchColor)),
         torchColor,
         0.5 + 0.5*COLORED_LIGHT_STRENGTH
      ));

      torchColor *= torchPeak / max(max(torchColor.r, max(torchColor.g, torchColor.b)), 1.0e-4);
   #endif

   return max(0.0, 1.0 - luma(ambient)) * strength * TORCH_CONTRAST_GAIN * torchColor;
}

// KEEP IN SYNC with common/getDiffuse.vsh (viewNormal replaces
// gl_NormalMatrix * gl_Normal)
float voxyDiffuse(vec3 viewNormal, bool isThin, float fogMix) {
   return (isEyeInWater == 0 ? 1.0 : 0.5)
        * (1.0 - fogMix)
        * (1.0 - rainStrength)
        * (isThin ? 0.75 : clamp(2.5*dot(viewNormal, normalize(shadowLightPosition)), -0.3333, 1.0));
}
