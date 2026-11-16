#ifndef VOXY_PATCH
uniform vec3 fogColor;
uniform vec3 skyColor;
#endif

#include "/common/getSkyTint.glsl"

vec3 getFogColor(float fogMix, vec3 feetPos, float sunH) {
   #ifdef OVERWORLD
      return mix(fogColor, skyColor, fogMix * clamp(0.006*feetPos.y, 0.0, 1.0)) * getSkyTint(sunH);
   #else
      return fogColor;
   #endif
}
