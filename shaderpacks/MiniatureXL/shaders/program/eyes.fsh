// emissive entity layers (see eyes.vsh): drawn as before, and they glow.
// the layer IS the emissive mask - only its lit texels exist (the rest is
// black or transparent), so every visible texel glows, in its own color

#include "/shader.h"

uniform sampler2D gtexture;
uniform sampler2D lightmap;

varying vec2 lightUV;
varying vec2 texUV;
varying vec4 color;

#ifdef BLOOM
   varying float bloomDistFade;

   #include "/common/math.glsl"
#endif

void main() {
   vec4 albedo = texture2D(gtexture, texUV)
               * texture2D(lightmap, lightUV)
               * color;

   #ifdef BLOOM
      /* DRAWBUFFERS:02 */
   #else
      /* DRAWBUFFERS:0 */
   #endif

   gl_FragData[0] = albedo;

   #ifdef BLOOM
      float albedoLuma = luma(albedo.rgb);

      // black texels of an additive eyes layer add nothing and must not
      // glow. judged on the brightest channel: a deep red eye has a luma
      // of ~0.15 but is fully lit
      float peakChannel = max(albedo.r, max(albedo.g, albedo.b));
      float level = smoothe(rescale(peakChannel, 0.08, 0.4)) * albedo.a * GLOW_MOBS;

      // brightness-normalized like the block glow: dim purple enderman eyes
      // glow as clearly as bright spider eyes
      vec3 glowColor = albedo.rgb * min(4.0, max(1.0, 0.5 / max(albedoLuma, 0.001)));

      gl_FragData[1] = vec4(clamp(glowColor, 0.0, 1.0) * (level * bloomDistFade), 1.0);
   #endif
}
