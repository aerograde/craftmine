#include "/shader.h"

#ifdef HDR
/*
const int colortex0Format = RGBA16F;
*/
#endif

#ifdef BLOOM
/*
const bool colortex3MipmapEnabled = true;
const int colortex2Format = RGBA16F;
*/
#endif

uniform float far;
uniform int isEyeInWater;
uniform sampler2D colortex0;
uniform sampler2D colortex6;
uniform sampler2D colortex7;
uniform sampler2D depthtex0;

#ifdef BLOOM
   uniform float viewWidth;
   uniform float viewHeight;
   uniform sampler2D colortex2;
   uniform sampler2D colortex3;
#endif

#if defined SUN_SHAFTS && defined OVERWORLD && defined SUN_GLOW
   uniform float rainStrength;
   uniform vec3 sunPosition;
#endif

varying vec2 texUV;

#include "/common/math.glsl"
#include "/common/transformations.glsl"
#include "/common/getReflectionColor.fsh"

#ifdef BLOOM
   // one level of the wide halo: the mip chain of the blurred glow
   // (colortex3MipmapEnabled - Iris builds it right before this pass).
   // texture2D's bias selects the level: colortex3 is 1/BLOOM_DOWNSCALE of
   // the screen, so plain sampling sits at lod -log2(BLOOM_DOWNSCALE).
   // four bilinear taps around the pixel smooth the coarse level into a
   // tent, so no blocky mip footprint shows
   vec3 bloomLevel(float lod) {
      float bias = lod + log2(BLOOM_DOWNSCALE);
      vec2 d = 0.75 * exp2(lod) * BLOOM_DOWNSCALE / vec2(viewWidth, viewHeight);

      return 0.25 * (texture2D(colortex3, texUV + vec2(-d.x, -d.y), bias).rgb
                   + texture2D(colortex3, texUV + vec2( d.x, -d.y), bias).rgb
                   + texture2D(colortex3, texUV + vec2(-d.x,  d.y), bias).rgb
                   + texture2D(colortex3, texUV + vec2( d.x,  d.y), bias).rgb);
   }
#endif

void main() {
   vec4 color = texture2D(colortex0, texUV);

   #ifdef HDR
      // a float buffer keeps what RGBA8 silently clamped: negative values
      // and NaN from any upstream math. drop them here (comparisons are
      // false for NaN)
      color.rgb = vec3(color.r >= 0.0 ? color.r : 0.0,
                       color.g >= 0.0 ? color.g : 0.0,
                       color.b >= 0.0 ? color.b : 0.0);
   #endif
   vec4 reflectivityAndRoughness = texture2D(colortex7, texUV);
   float reflectivity = reflectivityAndRoughness.x;
   float roughness = reflectivityAndRoughness.y;

   if (reflectivity > MIN_REFLECTIVITY && abs(reflectivityAndRoughness.z - 0.5) < 0.01) {
      // the normal doesn't come premultiplied by the normal matrix to
      // avoid the modelview transformations when view bobbing is on
      // which causes severe artifacts when moving
      vec3 prenormal = screen2ndc(texture2D(colortex6, texUV).xyz);

      #if WATER_WAVE_SIZE > 0

         if ((abs(prenormal.x) > 0.0 || abs(prenormal.z) > 0.0) && abs(prenormal.y) > 0.3333) {
            prenormal.xyz *= 1.0 / prenormal.y;
            prenormal.xz *= 0.01 * WATER_WAVE_SIZE;
         }

      #endif

      float depth  = texture2D(depthtex0, texUV).x;
      vec3 normal  = eye2view(prenormal);
      vec3 viewPos = screen2view(texUV, depth);
      vec3 feetPos = view2feet(viewPos);
      vec3 worldPos = feet2world(feetPos);
      float pixelDistance = min(1.0, length(feetPos)/16.0);
      float stepSize = stepify(mix(1.0/512.0, 1.0/64.0, pixelDistance), 1.0/512.0);

      normal += roughness * random3(stepify(worldPos, stepSize));
      normal = normalize(normal);

      vec4 reflectionColor = getReflectionColor(depth, normal, viewPos);

      color.rgb = mix(
         color.rgb,
         reflectionColor.rgb,
         reflectionColor.a * reflectivity * 0.1*REFLECTIONS * waterEdgeFade(feetPos, far)
      );
   }

   #if defined SUN_SHAFTS && defined OVERWORLD && defined SUN_GLOW
      if (rainStrength < 0.999) {
         // sun by day, moon by night
         vec3 srcPos = sunPosition;
         vec3 shaftTint = vec3(1.0, 0.85, 0.6);
         float shaftBase = 0.24;
         float upness = view2eye(normalize(sunPosition)).y;

         if (upness < 0.0) {
            srcPos = -sunPosition;
            shaftTint = vec3(0.55, 0.65, 1.0);
            shaftBase = 0.12;
            upness = -upness;
         }

         float horizonFade = rescale(upness, 0.0, 0.15);

         if (horizonFade > 0.0 && srcPos.z < 0.0) {
            vec3 srcDir = normalize(srcPos);
            vec3 viewDir = normalize(screen2view(texUV, 1.0));
            float closeness = max(0.0, dot(viewDir, srcDir));

            if (closeness > 0.5) {
               // march toward the light accumulating sky visibility (crepuscular rays)
               vec2 srcScreen = view2screen(srcPos).xy;
               vec2 stepUV = (srcScreen - texUV) / 12.0;
               vec2 uv = texUV + stepUV * random(texUV); // dither hides banding

               float shaft = 0.0;
               float weight = 1.0;

               for (int i = 0; i < 12; i++) {
                  uv += stepUV;

                  vec2 cuv = clamp(uv, 0.0, 1.0);
                  shaft += weight * step(1.0, texture2D(depthtex0, cuv).x);
                  weight *= 0.93;
               }

               shaft *= pow3(rescale(closeness, 0.5, 1.0)) / 12.0;

               color.rgb += shaftTint
                  * (shaftBase * SUN_SHAFTS_STRENGTH * shaft * horizonFade * (1.0 - rainStrength));
            }
         }
      }
   #endif

   #ifdef BLOOM
      // per-pixel glow mask: colortex2 holds each pixel's OWN glow (its
      // emissive texels, per material, per texel), colortex7.a marks the
      // faces of light sources and glowing materials
      vec3 core = vec3(0.0);
      float coreDepth = texture2D(depthtex0, texUV).x;
      float portalFace = 0.0; // colortex2.a -1: nether portal (translucent.fsh)
      float beamFace = 0.0;   // colortex2.a -2: beacon beam (beaconbeam.fsh)

      if (coreDepth < 1.0) {
         // same near-field cap as the blur pass: distant/depthless geometry
         // carries no core either
         vec4 coreSample = texture2D(colortex2, texUV);

         core = coreSample.rgb
              * (1.0 - pow2(rescale(length(screen2view(texUV, coreDepth)), 96.0, 144.0)));
         portalFace = (coreSample.a < -0.5 && coreSample.a > -1.5) ? 1.0 : 0.0;
         beamFace = coreSample.a < -1.5 ? 1.0 : 0.0;
      }

      float ownGlow = smoothe(rescale(luma(core), 0.02, 0.2));
      float emitterFace = abs(reflectivityAndRoughness.a - 0.5) < 0.25 ? 1.0 : 0.0;

      #ifdef HDR
         // punch: a glowing texel's own glow is added to the scene BEFORE
         // the display mapping, so strong ones reach white-hot
         color.rgb += core * BLOOM_CORE_HDR;
      #endif
   #endif

   #ifdef HDR
      // HDR -> display. the 0..1 range is returned untouched (the pack's
      // look). past 1.0 a channel clips and its EXCESS spills into the
      // other channels, the way film overexposes: a warm color goes orange ->
      // yellow -> white, never pink. (mixing the hue toward white instead
      // turned orange into salmon, and washed lava out)
      {
         vec3 excess = max(color.rgb - 1.0, vec3(0.0));
         float peak = max(color.r, max(color.g, color.b));

         // spill shared in proportion to the SQUARE of each channel's
         // relative level: the channel closest to clipping takes the most,
         // a mid channel much less (linear shares turned every orange
         // emitter yellow: copper bulbs, lamps), white only at high excess
         vec3 share = color.rgb / max(peak, 1.0e-4);
         vec3 spill = HDR_SPILL * (excess.r + excess.g + excess.b) * share * share;

         color.rgb = min(min(color.rgb, vec3(1.0)) + spill, vec3(1.0));
      }
   #endif

   #ifdef BLOOM
      // multi-scale halo (Photon-like): the tight blur from the composite
      // passes (a few pixels) + two mip levels of it (~1/8 and ~1/32 of the
      // screen-space scale). the tight one alone was a few pixels wide: a
      // lamp read as a crisp core with no halo at all
      vec3 glow = texture2D(colortex3, texUV).rgb
                + (BLOOM_HALO_MID * BLOOM_HALO_SIZE) * bloomLevel(2.0)
                + (BLOOM_HALO_WIDE * BLOOM_HALO_SIZE * BLOOM_HALO_SIZE) * bloomLevel(4.0);

      // a glowing block's own dark texels take only part of the halo: they
      // stay readable (no flat wash over the face) but the glow still
      // visibly spills across it - cutting it to zero there erased the
      // halo on any wall of glowing blocks (warped stems, sculk)
      glow *= mix(1.0, mix(BLOOM_FACE_HALO, 1.0, ownGlow), emitterFace);

      // the halo belongs AROUND a source: on its own glowing texels it only
      // stacked on top of the core and clipped them to white (amethyst
      // clusters, end rods, whole copper bulbs)
      // except on the nether portal: there the halo IS the look - it fills
      // the portal and blends its glow into one soft violet blob
      // the beacon beam keeps most of it too: it IS a beam of light
      glow *= 1.0 - mix(mix(BLOOM_SOURCE_HALO_CUT * ownGlow, PORTAL_SOURCE_HALO_CUT, portalFace),
                        BEAM_SOURCE_HALO_CUT, beamFace);

      glow += core * mix(mix(BLOOM_CORE, PORTAL_CORE, portalFace), BEAM_CORE, beamFace);

      glow = clamp(glow * BLOOM_STRENGTH, 0.0, 1.0);

      // screen blend to avoid harsh clipping
      color.rgb = 1.0 - (1.0 - color.rgb) * (1.0 - glow);
   #endif

   gl_FragData[0] = color;
}