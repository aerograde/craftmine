// particles and other "textured" geometry: vanilla-equivalent shading, plus
// glow output for bright pixels (fire, flames, lava drips...). beacon beams
// have their own dedicated program (beaconbeam.fsh). this program is only
// enabled when BLOOM and BLOOM_PARTICLES are both on (see shaders.properties);
// otherwise these render vanilla as before.

#include "/shader.h"

uniform sampler2D gtexture;
uniform sampler2D lightmap;

varying float bloomDist;
varying vec2 lightUV;
varying vec2 texUV;
varying vec4 color;

#include "/common/math.glsl"

void main() {
   vec4 texel  = texture2D(gtexture, texUV);
   vec4 albedo = texel * color;

   float texLuma = luma(albedo.rgb);

   // warden sonic boom: Iris gives particles no id, so its rings are
   // recognized by their exact vanilla texel colors (particle/sonic_boom_*:
   // light cyan #2ce3eb, dark teal #0bb4aa; any other particle drawn in
   // exactly these colors would glow the same). they glow at 10%, the dark
   // teal by its lightness rank (~4%). needs Glow > Particles on
   vec3 sonicLight = vec3(44.0, 227.0, 235.0) / 255.0;
   vec3 sonicDark  = vec3(11.0, 180.0, 170.0) / 255.0;
   float sonicLevel = max(
      0.10 * (1.0 - step(0.02, distance(texel.rgb, sonicLight))),
      0.04 * (1.0 - step(0.02, distance(texel.rgb, sonicDark))));

   albedo *= texture2D(lightmap, lightUV);

   /* DRAWBUFFERS:02 */
   gl_FragData[0] = albedo;

   // only bright pixels glow, scaled down so a screenful of particles does not
   // overwhelm the bloom buffer. the hard distance cap keeps anything routed
   // through this fallback program at range (Voxy LOD terrain) out of the glow -
   // particles are near the player by nature
   float bloomFade = 1.0 - rescale(bloomDist, 96.0, 144.0);

   float particleGlow = 0.8 * pow2(rescale(texLuma, 0.6, 0.9));

   vec3 glowColor = albedo.rgb;

   if (sonicLevel > 0.0) {
      // fixed level, whatever the particle's fade: the sonic boom ring
      // glows in its own cyan, brightness-normalized like block glow
      particleGlow = sonicLevel * GLOW_MOBS;
      glowColor = clamp(texel.rgb * min(4.0, max(1.0, 0.5 / max(luma(texel.rgb), 0.001))), 0.0, 1.0);
   }

   gl_FragData[1] = vec4(glowColor * (particleGlow * bloomFade), albedo.a);
}
