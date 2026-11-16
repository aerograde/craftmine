#include "/shader.h"

uniform float screenBrightness;
uniform int entityId;
uniform ivec2 eyeBrightnessSmooth;
uniform sampler2D gtexture;
uniform sampler2D lightmap;
uniform vec4 entityColor;

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
#endif

#ifdef GLOWING_ORES
   flat varying float isOre;
#endif

#ifdef BLOOM
   #define BLOOM_PASS
#endif

#ifdef BLOOM_PASS
   #ifdef GBUFFERS_TERRAIN
      // size of the bound texture if it is an atlas, 0 otherwise: the font
      // pages of sign text are no atlas (glow ink text below)
      uniform ivec2 atlasSize;
   #endif

   varying float bloomDistFade;
   flat varying float bloomOverlayOk;
   flat varying float bloomCutoff;
   flat varying float bloomLevelCap;
   flat varying vec3 bloomTint;
   flat varying float glowScale;
#endif

#if HAND_DYNAMIC_LIGHTING == 0
   uniform int heldBlockLightValue;
#elif HAND_DYNAMIC_LIGHTING == 1
   #define heldBlockLightValue 14
#endif

#if defined COLORED_LIGHT_ACTIVE && !defined ENABLE_SHADOWS
   // the Nether and End (and the Overworld with shadows off) cast no shadows, so nothing there samples the shadow
   // map - and Iris SKIPS rendering a shadow pass it believes unused,
   // starving the colored light voxelizer. this sampler (tapped invisibly in
   // main) forces the pass to run
   uniform sampler2D shadowcolor0;
#endif

#ifdef CL_LAVA_TEXTURE
   flat varying float lavaRecolor;
#endif

#ifdef COLORED_LIGHT_ACTIVE
   varying vec3 lpvNormal;
#else
   const vec3 lpvNormal = vec3(0.0);
#endif

flat varying vec4 emissive;
flat varying float emissiveHalo;
flat varying float emissiveGlowGain;

#include "/common/math.glsl"
#include "/common/transformations.glsl"
#include "/common/lightPalette.glsl"
#include "/common/getTorchColor.fsh"
#include "/common/getAmbientColor.glsl"

#ifdef ENABLE_SHADOWS
   uniform mat4 shadowModelView;
   uniform mat4 shadowProjection;
   uniform sampler2D shadowtex1;
   uniform sampler2D shadowcolor0; // .a: occluder shadow strength (entity fade)

   varying vec3 lightColor;
   varying float diffuse;
   flat varying float shadowOpacity;

   #include "/common/getLightStrength.fsh"
#endif

void main() {
   if (fogMix > 0.999) {
      discard;
   }

   vec4 albedo = texture2D(gtexture, texUV);
   vec4 color  = color;

   // glowing materials: texels of the material's glow hue (or brightness)
   // shine at full brightness whatever the lighting, and glow (Photon-like)
   // judged on the tinted texel at full brightness: redstone wire is a gray
   // texture tinted red, and directional shading must not change the mask
   vec3 emissiveTexel = albedo.rgb * color.rgb / max(max(color.r, max(color.g, color.b)), 0.05);
   float emissiveMask = getEmissiveMask(emissiveTexel, emissive);

   #ifdef GBUFFERS_TERRAIN
      float ambientOcclusion = color.a;
      color.a = 1.0;
   #else
      float ambientOcclusion = 1.0;
   #endif

   #if defined CL_LAVA_TEXTURE && defined GBUFFERS_TERRAIN
      // lava recolor: with a shaderpack, the resource pack's biome lava tint
      // arrives as a plain multiply (texture x tint) - two saturated colors
      // multiplied crush the texture pattern into a flat sheet. recolor as
      // hue x texture-luminance instead: the tint gives the color, the
      // texture keeps its full pattern contrast (matches the no-shader look)
      // chroma gate: vanilla lava carries NO biome tint (vertex color is
      // white), and a white tint would desaturate the texture to gray here.
      // only recolor when a resource pack actually supplies a chromatic tint
      float tintChroma = max(color.r, max(color.g, color.b))
                       - min(color.r, min(color.g, color.b));

      if (lavaRecolor > 0.5 && tintChroma > 0.06) {
         vec3 tintHue = color.rgb / max(max(color.r, max(color.g, color.b)), 0.05);

         // hue swap: chromaticity from the biome tint, pattern from the
         // texture's own luminance. restoring the luminance EXACTLY would
         // divide by luma(tintHue) - 0.58 for a saturated orange, a x1.7 gain
         // that drives the red channel past 1.0 over most of the texture and
         // clips it to a flat sheet ("everything so orange and uniform").
         // the gain is capped so the brightest channel stays inside range and
         // the whole texture pattern survives
         float tintGain = min(1.0 / max(luma(tintHue), 0.05), 1.25);

         albedo.rgb = min(vec3(1.0), tintHue * (luma(albedo.rgb) * tintGain));
         color.rgb = vec3(1.0);
      }
   #endif

   #ifdef MOD_COLORWHEEL
      vec2 lightUV;
      vec4 entityColor;

      clrwl_computeFragment(albedo, albedo, lightUV, ambientOcclusion, entityColor);
   #endif

   vec4 ambient = getAmbientColor(lightUV.t, sunHeight);

   #ifdef THE_END
      ambient.rgb *= endTint;
   #endif

   #ifdef GLOWING_ORES
      ambient.rgb = mix(
         ambient.rgb,
         vec3(1.0, 0.9, 0.9),
         isOre * 0.3333*squaredLength(rescale(albedo.rgb, vec3(0.59), vec3(1.0)))
      );
   #endif

   albedo *= color;

   // render thunder
   albedo.a = entityId == 11000.0 ? 0.15 : albedo.a;
   // render entity color changes (e.g taking damage)
   albedo.rgb = mix(albedo.rgb, entityColor.rgb, entityColor.a);

   float albedoLuma = luma(albedo.rgb);
   float albedoPeak = max(albedo.r, max(albedo.g, albedo.b)); // unlit, like albedoLuma

   #ifdef ENABLE_SHADOWS
      float lightStrength = getLightStrength(diffuse, lightUV.t, feetPos);
      float lightBrightness = max(0.0, LIGHT_BRIGHTNESS - 0.5*pow3(albedoLuma));

      lightStrength = max(lightStrength, 0.75*lightSourceLevel);

      // time-of-day opacity applied to the darkening itself: lit areas keep
      // their full brightness, only how DARK a shadow gets is scaled
      ambient.rgb *= mix(mix(vec3(1.0), SHADOW_COLOR, shadowOpacity), vec3(1.0), lightStrength);
      ambient.rgb *= 0.75 + (lightBrightness * lightStrength) * lightColor;
   #endif

   ambient.rgb = contrastAmbient(ambient.rgb);
   ambient.rgb += getTorchColor(lightUV.s, ambient.rgb, feetPos, lpvNormal);

   // fullbright overlay (resource pack emissive layer): Minecraft draws it at
   // FULL block AND sky light. block light alone is not enough - a face
   // touching a level-15 source (lantern, glowstone) also reads block 15 and
   // was taken for an overlay: hue neutralized and glowing, an exposure jump
   // on that one face. a strong light-volume reading means a real source
   float overlayLight = rescale(lightUV.s, 0.935, 0.96) * rescale(lightUV.t, 0.935, 0.96)
                      * (1.0 - rescale(lpvStrengthOut, 0.5, 0.8));

   #if defined BLOOM_PASS && defined GBUFFERS_TERRAIN && defined BLOOM_EMISSIVE_TEXTURES
      // resource pack emissive overlays are SELF-lit: they must keep their own
      // texture color instead of taking the hue of nearby light sources (white
      // quartz sparks turning cyan next to cyan lava). same mask as the
      // overlay glow: fullbright lightmap on a non-emitting block. the ambient
      // is neutralized in hue at identical peak brightness
      float selfLit = overlayLight * bloomOverlayOk
                    * float(lightSourceLevel <= 0.12);

      ambient.rgb = mix(ambient.rgb, vec3(max(ambient.r, max(ambient.g, ambient.b))), selfLit);
   #endif

   // self-lit emissive texels: never darker than their own glow
   // only the HOTTEST texels (lightness 0.7+: flame cores, lamp filaments)
   // are pushed past 1.0 into white-hot; the colored body of a glowing
   // texture (lava, crimson veins) stays at most fully lit, so it keeps its
   // color instead of washing out
   ambient.rgb = max(ambient.rgb, vec3(emissiveMask * emissiveHeat(albedo.rgb)));

   albedo.rgb *= ambientOcclusion;

   albedo *= ambient;

   // enchantment glint. on MC 26.x Iris draws enchanted items and armor in
   // ONE pass through gbuffers_entities / gbuffers_hand (inline glint) - the
   // separate gbuffers_armor_glint pass no longer runs for them, so a pack
   // that does not sample the glint here shows none. mc_hasGlint() and
   // mc_sampleGlint() are injected by Iris (stubs returning false / 0 in
   // every non-glint program). vanilla blends the glint (SRC_COLOR, ONE):
   // it ADDS glint^2, unlit, after lighting - reproduced exactly
   vec3 glint = vec3(0.0);

   #ifdef IRIS_INLINE_GLINT
      if (mc_hasGlint()) {
         glint = mc_sampleGlint();
         glint *= glint;
         albedo.rgb += glint;
      }
   #endif

   #if defined THE_NETHER && defined NETHER_COLOR_GRADING
      albedo.rgb = mix(
         vec3(luma(albedo.rgb)),
         albedo.rgb,
         max(rescale(luma(ambient.rgb), 0.2, 0.6), rescale(albedoLuma, 0.15, 0.4))
      );
   #endif

   albedo.rgb = mix(albedo.rgb, gradientFogColor, fogMix);

   #if defined COLORED_LIGHT_ACTIVE && !defined ENABLE_SHADOWS
      // invisible shadow-map tap (see the sampler declaration above): the
      // 1e-6 contribution cannot be folded away, so the sampler stays active
      // and Iris keeps the Nether/End shadow pass (our voxelizer) alive
      albedo.rgb += texture2D(shadowcolor0, vec2(0.5)).rgb * 1.0e-6;
   #endif

   #ifdef BLOOM_PASS
      /* DRAWBUFFERS:0672 */
   #else
      /* DRAWBUFFERS:067 */
   #endif

   gl_FragData[0] = albedo;

   // colortex7.a on the face of a light source or glowing material:
   // 0.3 + 0.4 x its halo amount (0.3..0.7); 1.0 elsewhere, sky reads 0.
   // composite.fsh scales the halo it spreads, final.fsh keeps the halo off
   // the face's own non-glowing texels
   #ifdef GBUFFERS_TERRAIN
      float emitterSurfaceMark = (lightSourceLevel > 0.12 || emissive.w > 0.0)
                               ? 0.3 + 0.4 * emissiveHalo : 1.0;
   #else
      float emitterSurfaceMark = 1.0;
   #endif

   #if BLOCK_REFLECTIONS > 0
      float reflectivity = max(0.0, (albedoLuma - blockReflectivity.y) * blockReflectivity.x * 0.2*BLOCK_REFLECTIONS);

      gl_FragData[1] = vec4(normal, 1.0);
      gl_FragData[2] = vec4(reflectivity, blockReflectivity.z, 0.5, emitterSurfaceMark);
   #else
      gl_FragData[1] = vec4(vec3(0.0), 1.0);
      gl_FragData[2] = vec4(vec3(0.0), emitterSurfaceMark);
   #endif

   #ifdef BLOOM_PASS
      // selective glow: on light-emitting blocks, only the texture's bright
      // (actually emitting) pixels glow - torch flame yes, torch stick no.
      // every source type gets the same strength once its pixels qualify
      // the emissive model below ADDS to this (max): replacing it lost the
      // glow of crystals and the brewing stand's rod, whose texels the
      // tuned luma cutoffs catch better on resource-pack textures
      // light sources WITHOUT an emissive model only: blocks that have one
      // glow exactly on its texels (below), never by brightness alone
      float bloomLevel = lightSourceLevel > 0.12 && emissive.w <= 0.0
         ? smoothe(rescale(albedoLuma, bloomCutoff, bloomCutoff + 0.2))
         : 0.0;

      #ifdef BLOOM_EMISSIVE_TEXTURES
         // resource pack emissive overlays are an exact per-pixel mask, but ONLY
         // on non-emitting blocks: a level-15 emitter's own geometry (lantern,
         // campfire) is also rendered fullbright, so the mask cannot separate
         // body from flame there - those use the per-category cutoff above.
         // bloomOverlayOk excludes foreign LOD geometry, which is fullbright too
         if (lightSourceLevel <= 0.12 && bloomOverlayOk > 0.25) {
            float overlayTerm = overlayLight;

            if (bloomOverlayOk < 0.75) {
               // entities: only vivid emissive pixels (glowing eyes...)
               // qualify. judged on the brightest CHANNEL, not luma: red
               // spider eyes or purple enderman eyes (resource-pack
               // emissive layers drawn fullbright, Fresh Animations/ETF)
               // have a luma of ~0.3 and never passed a luma test
               overlayTerm *= smoothe(rescale(albedoPeak, 0.45, 0.75));
            }

            bloomLevel = max(bloomLevel, overlayTerm);
         }
      #endif

      #ifdef GLOWING_ORES
         // masked to the bright ore bits only, never the whole block face
         // brightest channel, not luma: lapis blue and redstone red ore
         // bits have a low luma; gray stone/deepslate stays below 0.6
         bloomLevel = max(bloomLevel, isOre * 0.3 * smoothe(rescale(albedoPeak, 0.6, 0.9)));
      #endif

      // glowing materials: their emissive texels glow, whatever their luma
      bloomLevel = max(bloomLevel, min(emissiveMask * emissiveGlowGain, 3.0));

      #ifdef GBUFFERS_TERRAIN
         // glow ink sign text. Iris draws block-entity text with
         // gbuffers_block_translucent, which falls back to this program: no
         // mid-block data (bloomOverlayOk 0), a font page instead of an atlas
         // (atlasSize 0), and glowing text is drawn FULLBRIGHT (block and sky
         // light 15) while plain text takes the sign's own light. a light,
         // readable glow: GLOW_INK_TEXT on the glyph's lightest pixels
         float glowInk = float(atlasSize.x == 0 && bloomOverlayOk < 0.25)
                       * rescale(lightUV.s, 0.935, 0.96) * rescale(lightUV.t, 0.935, 0.96);

         bloomLevel = max(bloomLevel, GLOW_INK_TEXT * glowInk * smoothe(rescale(albedoPeak, 0.1, 0.5)));
      #endif

      // per-category glow ceiling, applied AFTER every glow path (emitter,
      // overlay, ores) so nothing can max() past it: keeps uniformly bright
      // retextured lava from whiting out, and softens the fullbright sculk
      // sensor tendrils
      bloomLevel = min(bloomLevel, bloomLevelCap);

      // Glow per block slider of this block's category (0 = no glow)
      bloomLevel *= glowScale;

      // brightness-normalize the glow color so dark emissive textures (soul fire,
      // sculk, portal...) glow as strongly as bright ones (torch flame)
      vec3 bloomColor = albedo.rgb * min(4.0, max(1.0, 0.5 / max(albedoLuma, 0.001)));

      // vein mode (warped stems): glow color from the UNLIT texel. the lit
      // albedo carries the self-light (= the mask in the dark), so the glow
      // went as mask^2 and the plain veins at 55% glowed ~30%. unlit, the
      // glow follows the mask linearly and the same in light or dark
      if (emissive.x < -5.5) {
         bloomColor = emissiveTexel * min(4.0, max(1.0, 0.5 / max(luma(emissiveTexel), 0.001)));
      }

      bloomColor *= bloomTint; // per-category hue correction (copper -> green)

      vec3 bloomOut = clamp(bloomColor, 0.0, 1.0) * (bloomLevel * (1.0 - fogMix) * bloomDistFade);

      // the glint glows in its own color and pattern (a resource pack's
      // purple glint glows purple), brightness-normalized like the rest:
      // the glint streaks are dim, they would barely register otherwise.
      // Enchantment glint glow slider: 0 (the default) = no glint glow
      float glintLuma = luma(glint);
      vec3 glintGlow = glint * min(4.0, 0.5 / max(glintLuma, 0.001))
                     * (0.7 * ENCHANT_GLOW * smoothe(rescale(glintLuma, 0.01, 0.12)));

      bloomOut = max(bloomOut, clamp(glintGlow, 0.0, 1.0) * ((1.0 - fogMix) * bloomDistFade));

      gl_FragData[3] = vec4(bloomOut, albedo.a);
   #endif
}