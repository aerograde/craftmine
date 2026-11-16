#include "/shader.h"

attribute vec4 mc_Entity;

#ifdef IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE
   attribute vec4 at_midBlock;
#endif

uniform float rainStrength;
uniform float screenBrightness;
uniform int isEyeInWater;
uniform int worldTime;
uniform vec3 sunPosition;

#ifdef HIGHLIGHT_WAXED
   uniform int heldItemId;
   uniform int heldItemId2;
#endif

flat varying float lightSourceLevel;
varying float fogMix;
varying float sunHeight;
varying vec2 lightUV;
varying vec2 texUV;
varying vec3 feetPos;
varying vec3 gradientFogColor;

#ifdef GBUFFERS_TERRAIN
   varying vec4 color;
#else
   flat varying vec4 color;
#endif

#ifdef THE_END
   varying vec3 endTint;
#endif

#if BLOCK_REFLECTIONS > 0
   flat varying vec3 blockReflectivity;
   varying vec3 normal;

   #include "/common/blockReflectivity.glsl"
#endif

#ifdef GLOWING_ORES
   flat varying float isOre;
#endif

#ifdef BLOOM
   #define BLOOM_PASS
#endif

#ifdef BLOOM_PASS
   varying float bloomDistFade;
   flat varying float bloomOverlayOk;
   flat varying float bloomCutoff;
   flat varying float bloomLevelCap;
   flat varying vec3 bloomTint;
   flat varying float glowScale; // per-category glow slider (getGlowScale)
#endif

#if !defined GBUFFERS_TERRAIN && !defined GBUFFERS_HAND
   uniform int entityId;
#endif

#ifdef CL_LAVA_TEXTURE
   flat varying float lavaRecolor;
#endif

#ifdef COLORED_LIGHT_ACTIVE
   varying vec3 lpvNormal; // world-space surface normal (colored light lookup)
#endif

// glowing materials: which texels shine and glow (lightPalette.glsl)
flat varying vec4 emissive;
flat varying float emissiveHalo; // halo amount of this material (getEmissiveGlow)
flat varying float emissiveGlowGain; // emissive mask -> glow level (see below)

#include "/common/math.glsl"
#include "/common/lightPalette.glsl"
#include "/common/lightContrast.glsl"
#include "/common/transformations.glsl"
#include "/common/getFogMix.vsh"
#include "/common/getFogColor.vsh"
#include "/common/getViewPosition.vsh"
#include "/common/waving.glsl"

#if defined GBUFFERS_TERRAIN && defined WAVING_ACTIVE
   uniform float frameTimeCounter;
#endif

#ifdef ENABLE_SHADOWS
   uniform vec3 shadowLightPosition;

   varying vec3 lightColor;
   varying float diffuse;
   flat varying float shadowOpacity;

   #include "/common/getDiffuse.vsh"
   #include "/common/getLightColor.vsh"
#endif

void main() {
   gl_Position = ftransform();

   #if defined GBUFFERS_TERRAIN && defined WAVING_ACTIVE
      {
         // waving leaves and plants (common/waving.glsl), same offset as
         // in shadow.vsh so shadows follow
         vec3 wavingWorld = view2feet((gl_ModelViewMatrix * gl_Vertex).xyz) + cameraPosition;
         float wavingSky = (gl_TextureMatrix[1] * gl_MultiTexCoord1).t;

         #ifdef IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE
            bool wavingMid = dot(at_midBlock.xyz, at_midBlock.xyz) > 1.0
                          && all(lessThan(abs(at_midBlock.xyz), vec3(40.0)));
            float wavingMidY = at_midBlock.y;
         #else
            bool wavingMid = false;
            float wavingMidY = 0.0;
         #endif

         vec3 wave = getWaving(int(mc_Entity.x), wavingWorld, wavingMidY, wavingMid,
                               wavingSky, frameTimeCounter, rainStrength);

         if (dot(wave, wave) > 0.0) {
            gl_Position = gl_ModelViewProjectionMatrix * vec4(gl_Vertex.xyz + wave, 1.0);
         }
      }
   #endif

   sunHeight = view2feet(sunPosition).y;
   color     = gl_Color;
   texUV     = (gl_TextureMatrix[0] * gl_MultiTexCoord0).st;
   lightUV   = (gl_TextureMatrix[1] * gl_MultiTexCoord1).st;

   #ifdef IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE
      lightSourceLevel = at_midBlock.w < 16.0 ? at_midBlock.w / 15.0 : 0.0;
   #else
      lightSourceLevel = float(mc_Entity.x == 10068.0 || mc_Entity.x == 10072.0 || mc_Entity.x == 10260.0);
   #endif

   #ifdef GBUFFERS_TERRAIN
      emissive = getEmissiveParams(int(mc_Entity.x));
      emissiveHalo = getEmissiveGlow(int(mc_Entity.x));

      #ifdef IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE
         // groups whose emission depends on the block state (furnaces,
         // redstone torch and lamp, lit ore...) only shine while they emit
         // amethyst blocks (10261) glow without emitting anything
         if (lightSourceLevel <= 0.0 && getLightEmitFloor(int(mc_Entity.x)) <= 0.0
          && mc_Entity.x != 10261.0) {
            emissive.w = 0.0;
         }

         // copper bulbs: the core's lightest texel drops with oxidation
         // (vanilla L 0.91 / 0.83 / 0.72 / 0.61 at light 15 / 12 / 8 / 4).
         // the 100% point follows the bulb's own light level, so the
         // oxidized bulb's core glows as fully as the fresh one's
         if ((mc_Entity.x == 20007.0 || mc_Entity.x == 20008.0) && lightSourceLevel > 0.0) {
            emissive.y = 0.501 + 0.41 * lightSourceLevel;
            emissive.z = emissive.y - 0.32;
         }
      #endif

      // glow of the emissive texels: the emissive mask is now the ONLY glow
      // mask of these blocks (the luma cutoff lit whole crystals, bulbs and
      // lichen patches). real light sources keep the glow level the luma
      // path gave their bright texels (1.0): the mask is normalized by its
      // strength. materials that only glow (sculk, stems) keep strength
      emissiveGlowGain = (lightSourceLevel > 0.12 || mc_Entity.x == 10212.0 || mc_Entity.x == 10225.0)
                       ? 1.0 / max(emissive.w, 0.05) : 1.0;

      // per-material glow trims (user tuning): end rods half. the nether
      // stems lost their x2: their vein masks peak at 1.0 on the animated
      // pulse (100%) and their glow color is taken unlit (textured_lit.fsh)
      if (mc_Entity.x == 10213.0) emissiveGlowGain *= 0.5;                          // end rod
      if (mc_Entity.x == 10261.0) emissiveGlowGain *= 0.8;                          // amethyst blocks: lightest facets 80%
      if (mc_Entity.x == 10212.0) emissiveGlowGain *= 2.0;                          // brewing stand rod
      if (mc_Entity.x == 10226.0) emissiveGlowGain *= 3.0;                          // eyeblossom eye (2x2 texels)

      // glowing materials count as (weak) light sources even where vanilla
      // gives them no emission: sculk, amethyst, powered redstone...
      lightSourceLevel = max(lightSourceLevel, getLightEmitFloor(int(mc_Entity.x)));
   #else
      emissive = vec4(0.0);
      emissiveHalo = 1.0;
      emissiveGlowGain = 1.0;

      #ifndef GBUFFERS_HAND
         // glowing mobs without a fullbright lightmap: the allay (vanilla
         // draws it at block light 15 only - sky light stays, so the
         // fullbright-overlay detection misses it under any roof). its light
         // body is self-lit and glows; it casts no colored light
         // (entity.properties: not in the light-casting group)
         // also blazes and magma cubes (10304), glow squids (10305) - their
         // fullbright look needs full SKY light too, so under a roof only
         // this mask keeps their hot parts / spots glowing
         // and the creaking (10307): its orange eye texels
         if (entityId >= 10304 && entityId <= 10307) {
            emissive = getEmissiveParams(entityId);
         }
      #endif
   #endif

   #ifdef THE_END
      endTint = END_AMBIENT + 0.02*(gl_NormalMatrix * gl_Normal).xyz;
   #endif

   #if BLOCK_REFLECTIONS > 0
      normal = ndc2screen(gl_Normal);
      blockReflectivity = BLOCK_REFLECTIVITY[int(max(0.0, mc_Entity.x - 20000.0))];
   #endif

   #ifndef CL_LAVA_TEXTURE
      // stylized vanilla lava: remap the vertex color toward white. MUST stay
      // disabled when following the texture/tint: resource packs (Nature X)
      // recolor lava through this very vertex tint, and the remap would crush
      // their color to near-white
      if (mc_Entity.x == 10068.0) { // lava
         color.rgb = mix(vec3(0.8, 0.5, 0.3), vec3(1.0), rescale(color.rgb, vec3(0.54), vec3(0.9)));
      }
   #endif

   #ifdef GLOWING_ORES
      // redstone ore lives in its own colored-light id (10220) but must keep
      // the ore glow: a block can only belong to ONE block.properties group
      isOre = float(mc_Entity.x == 10014.0 || mc_Entity.x == 10220.0);
   #endif

   #ifdef CL_LAVA_TEXTURE
      lavaRecolor = float(mc_Entity.x == 10068.0);
   #endif

   #ifdef HIGHLIGHT_WAXED
      color.rgb *= (heldItemId == 20007 || heldItemId2 == 20007) && mc_Entity.x == 20007.0 ? 0.4 : 1.0;
   #endif

   bool isThin = mc_Entity.x == 10031.0 || mc_Entity.x == 10059.0
              || (mc_Entity.x >= 10250.0 && mc_Entity.x <= 10252.0) // waving groups
              || mc_Entity.x == 10227.0 // glow berry vines
              || mc_Entity.x == 10253.0 // firefly bush
              || mc_Entity.x == 10175.0 || mc_Entity.x == 10176.0
              || mc_Entity.x == 10214.0 // glow lichen (moved out of 10059 for colored light)
              || mc_Entity.x == 10226.0 // open eyeblossom (moved out of 10059 to glow)
              || mc_Entity.x == 10228.0 // nether fungi, torchflower (same)
              || (abs(gl_Normal.y) < 0.01 && abs(abs(gl_Normal.x) - abs(gl_Normal.z)) < 0.01);

   #ifndef FLAT_LIGHTING
      vec3 lightNormal = gl_NormalMatrix * gl_Normal;

      lightNormal = isThin ? vec3(0.0, 1.0, 0.0) : view2feet(lightNormal);

      color.rgb *= clamp(lightNormal.x * lightNormal.x * 0.6
                       + lightNormal.y * lightNormal.y * 0.25 * (3.0 + lightNormal.y)
                       + lightNormal.z * lightNormal.z * 0.8, lightSourceLevel, 1.0);
   #endif

   feetPos = view2feet(getViewPosition());

   #ifdef COLORED_LIGHT_ACTIVE
      lpvNormal = mat3(gbufferModelViewInverse) * normalize(gl_NormalMatrix * gl_Normal);
   #endif

   #ifdef BLOOM_PASS
      // absolute distance cap on the CLIP-SPACE depth (gl_Position.w = true
      // perspective distance): unlike attributes, matrices or uniforms, this
      // cannot be faked by any renderer - geometry drawn at the horizon must
      // have a large w or it would not rasterize at the right size. reliably
      // excludes LOD terrain (Voxy etc.); glow is subpixel out there anyway
      bloomDistFade = 1.0 - rescale(max(gl_Position.w, length(feetPos)), 96.0, 144.0);

      // fullbright-overlay glow policy:
      //   terrain  -> full strength (1.0), the overlay is an exact mask - but only
      //               for geometry with plausible mid-block data; anything else
      //               (foreign LOD geometry) also gets its emission zeroed.
      //               Voxy LODs no longer come through here: they are shaded
      //               by program/voxy_*.glsl
      //   entities -> shaped by pixel brightness (0.5 marker), so emissive
      //               eyes glow but a mob standing in fullbright does not
      //   hand     -> never (0.0): dynamic hand light makes the arm fullbright
      #ifdef GBUFFERS_TERRAIN
         #ifdef IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE
            if (dot(at_midBlock.xyz, at_midBlock.xyz) > 1.0
             && all(lessThan(abs(at_midBlock.xyz), vec3(40.0)))) {
               bloomOverlayOk = 1.0;
            } else {
               bloomOverlayOk = 0.0;
               lightSourceLevel = 0.0;
            }
         #else
            bloomOverlayOk = 1.0;
         #endif
      #elif defined GBUFFERS_HAND
         bloomOverlayOk = 0.0;
      #else
         bloomOverlayOk = 0.5;
      #endif

      // per-category glow pixel cutoff (Complementary-style masks): blocks whose
      // body texture is bright (lantern cage, campfire logs) need a strict cutoff
      // so only their flame pixels glow; dim-textured emitters need a lenient one
      {
         int bloomId = int(mc_Entity.x);
         float cutoff = BLOOM_PIXEL_THRESHOLD;

         // lava & magma: fixed glow cap so a retextured, uniformly BRIGHT lava
         // (high luma) glows softly instead of whiting out. note: saturated
         // dark-red lavas sit below the luma cutoff and get no surface glow
         // at all - this cap is then irrelevant for them
         bloomLevelCap = (bloomId == 10068 || bloomId == 10208 || bloomId == 20001) ? 0.4 : 3.0; // > 1: per-material trims
         // (20001: the lava cauldron, same lava glow ceiling)

         // sculk sensor (10219; the calibrated ones have their own crystal
         // groups 10264-10266): its active tendrils render fullbright - kept
         // off the full-strength overlay path, and its cyan specks capped
         if (bloomId == 10219) {
            bloomOverlayOk = min(bloomOverlayOk, 0.5);
            bloomLevelCap = 0.5;
         }

         // per-category glow tint: the copper flame texture's near-white core
         // makes its bloom read yellow - pull the halo toward the green of
         // the light it casts
         bloomTint = bloomId == 10218 ? vec3(0.5, 1.0, 0.6)    // copper flame -> green
                   : bloomId == 10212 ? vec3(1.0, 0.62, 0.3)   // brewing stand rod -> orange
                   : vec3(1.0);

         // Glow per block sliders
         #ifdef GBUFFERS_TERRAIN
            glowScale = getGlowScale(bloomId, lightSourceLevel > 0.12);
         #elif defined GBUFFERS_HAND
            glowScale = 1.0;
         #else
            glowScale = GLOW_MOBS;
         #endif

         // the luma cutoff only shapes the glow of light sources WITHOUT an
         // emissive model (textured_lit.fsh): every palette category has one
         // now, so the old per-category cutoffs (flames, lamps, lava, stone-
         // bodied emitters...) were dead code. what remains: blocks that must
         // never glow, the brewing stand's forced emission, held items
         if (bloomId == 10209 || bloomId == 10262) cutoff = 9.9; // candles (the flame is a
                                     // particle), enchanting table: cast light, never glow
         if (bloomId == 10212) {
            // brewing stand ("alambic"): emits only light level 1, below the emitter
            // threshold - force it so the blaze rod always glows; the default cutoff
            // masks the rod (bright gold) from the stone base by itself
            lightSourceLevel = max(lightSourceLevel, 0.5);
         }

         #ifdef GBUFFERS_HAND
            // held items have no block id; most handheld lights are flame-bodied
            cutoff = BLOOM_PIXEL_THRESHOLD + 0.2;
         #endif

         bloomCutoff = clamp(cutoff, 0.05, 10.0); // above 1.0 = never glows (candles)
      }
   #endif

   fogMix = getFogMix(feetPos);
   fogMix = mix(fogMix, fogMix*fogMix, lightSourceLevel);
   gradientFogColor = getFogColor(fogMix, feetPos, sunHeight);

   #ifdef ENABLE_SHADOWS
      float skyLight = clamp(lightUV.t, 0.0, 1.0);

      diffuse = getDiffuse(sunHeight, skyLight, isThin);
      lightColor = getLightColor(sunHeight, skyLight);

      // time-of-day shadow opacity: day shadows at half the slider's darkness,
      // night (moon) shadows much fainter; narrow blend masks the sun/moon swap
      shadowOpacity = contrastShadowOpacity(mix(NIGHT_SHADOW, DAY_SHADOW, clamp(rescale(sunHeight, -3.0, 3.0), 0.0, 1.0)));
   #endif
}