// beacon beam: always emissive (no lightmap darkening - the beam is pure
// light), and a strong glow that follows the beam's own color (white core,
// stained-glass tinted beams stay tinted)

#include "/shader.h"

uniform sampler2D gtexture;

varying vec3 beamViewPos;
varying vec2 texUV;
varying vec4 color;

#include "/common/math.glsl"

void main() {
   vec4 albedo = texture2D(gtexture, texUV) * color;

   /* DRAWBUFFERS:02 */
   gl_FragData[0] = albedo;

   vec3 beamGlow = vec3(0.0);

   #ifdef BLOOM
      // the nether portal's logic (Photon-like): the glow follows each
      // texel's lightness on a near-linear curve (power 2.2) over the
      // texture's own range (vanilla beacon_beam.png: L 0.66-0.86) - the
      // darkest streaks ~6%, the bulk ~64%, the lightest ~118%: a contrasted
      // beam instead of a flat glowing bar. the color is the beam's own hue at
      // full brightness, so dark stained-glass beams (blue, black, brown)
      // glow as strongly as light ones. the translucent outer layer adds a
      // fraction (its alpha); colortex2 adds up (ONE ONE, shaders.properties)
      vec4 beamTexel = texture2D(gtexture, texUV);
      float beamL = 0.5 * (max(beamTexel.r, max(beamTexel.g, beamTexel.b))
                         + min(beamTexel.r, min(beamTexel.g, beamTexel.b)));
      float level = max(1.2 * pow(rescale(beamL, 0.60, 0.86), 2.2), 0.06);

      // distance cap PER FRAGMENT (see beaconbeam.vsh). 1/gl_FragCoord.w is
      // the clip-space w of this fragment: unfakeable, like the rest of the
      // pack's caps
      float bloomDist = max(1.0 / gl_FragCoord.w, length(beamViewPos));

      vec3 beamHue = albedo.rgb / max(max(albedo.r, albedo.g), max(albedo.b, 0.001));

      beamGlow = beamHue * (level * albedo.a * GLOW_LAMPS * (1.0 - rescale(bloomDist, 96.0, 144.0)));
   #endif

   // alpha -2: beam marker for final.fsh (keeps the halo over the beam)
   gl_FragData[1] = vec4(beamGlow, -2.0);
}
