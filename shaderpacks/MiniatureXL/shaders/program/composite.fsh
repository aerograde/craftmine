// bloom pass 1: bloom SOURCE at the bloom resolution (colortex3).
// evaluated once per low-res texel here, instead of once per blur tap in the
// blur pass (2*BLOOM_TAPS+1 times): the depth cap, the sun/moon halo and the
// saturation then cost one evaluation per texel. passes 2 and 3 blur it.

#include "/shader.h"

varying vec2 texUV;

#ifdef BLOOM

   uniform float viewWidth;
   uniform float viewHeight;
   uniform sampler2D colortex0;
   uniform sampler2D colortex2;
   uniform sampler2D colortex7;
   uniform sampler2D depthtex0;
   uniform vec3 fogColor;
   uniform vec3 sunPosition;

   #include "/common/math.glsl"
   #include "/common/transformations.glsl"

   vec3 getBloomSource(vec2 uv) {
      vec3 glow = texture2D(colortex2, uv).rgb;

      float depth = texture2D(depthtex0, uv).x;

      // authoritative near-field cap, enforced from the depth buffer: a draw
      // cannot fake its depth without breaking its own occlusion, so LOD
      // terrain (Voxy etc.) can never carry glow past this regardless of how
      // it feeds the gbuffer programs. no depth written -> no emissive glow
      if (depth < 1.0) {
         glow *= 1.0 - pow2(rescale(length(screen2view(uv, depth)), 96.0, 144.0));
      } else {
         glow = vec3(0.0);
      }

      // the sky path is a SUN/MOON HALO: it only applies in a tight cone around
      // the sun or moon direction
      #ifdef SUN_GLOW
         if (depth >= 1.0) {
            vec3 viewDir = normalize(screen2view(uv, 1.0));
            vec3 sunDir  = normalize(sunPosition);
            float proximity = max(dot(viewDir, sunDir), -dot(viewDir, sunDir)); // sun or moon

            float mask = rescale(proximity, 0.92, 0.99);

            if (mask > 0.0) {
               vec3 color = texture2D(colortex0, uv).rgb;

               // fog-colored pixels (haze) never glow
               mask *= rescale(length(color - fogColor), 0.05, 0.25);

               // soft knee above the threshold; safe across the whole slider range
               float highlight = pow2(rescale(luma(color), BLOOM_THRESHOLD, BLOOM_THRESHOLD + 0.2));

               glow = max(glow, color * (highlight * BLOOM_HIGHLIGHTS * mask));
            }
         }
      #endif

      // halo amount of the material this texel belongs to: faces of light
      // sources and glowing materials carry it in colortex7.a (0.3 + 0.4 x
      // amount); anything else halos fully. large glowing surfaces (lamps,
      // stems) spread less halo per texel - their own texels stay punchy
      float marker = texture2D(colortex7, uv).a;
      float halo = abs(marker - 0.5) < 0.25 ? clamp((marker - 0.3) / 0.4, 0.0, 1.0) : 1.0;

      glow *= halo;

      return max(vec3(0.0), mix(vec3(luma(glow)), glow, BLOOM_SATURATION));
   }

   void main() {
      #if BLOOM_QUALITY == 0
         // quarter resolution: each low-res texel covers 4x4 full-res pixels.
         // four bilinear taps on the inner corners average all sixteen, so a
         // small emitter can no longer fall between samples and flicker
         vec2 halfTexel = vec2(1.0 / viewWidth, 1.0 / viewHeight);

         vec3 source = 0.25 * (
              getBloomSource(texUV + vec2(-halfTexel.x, -halfTexel.y))
            + getBloomSource(texUV + vec2( halfTexel.x, -halfTexel.y))
            + getBloomSource(texUV + vec2(-halfTexel.x,  halfTexel.y))
            + getBloomSource(texUV + vec2( halfTexel.x,  halfTexel.y)));
      #else
         // half resolution: the texel center is the shared corner of its 2x2
         // full-res pixels, where one bilinear tap averages all four
         vec3 source = getBloomSource(texUV);
      #endif

      /* DRAWBUFFERS:3 */
      gl_FragData[0] = vec4(source, 1.0);
   }

#else

   void main() {
      /* DRAWBUFFERS:3 */
      gl_FragData[0] = vec4(0.0);
   }

#endif
