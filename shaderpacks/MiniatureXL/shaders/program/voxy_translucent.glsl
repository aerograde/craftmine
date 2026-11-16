#include "/program/voxy_common.glsl"
#include "/common/waterAlpha.glsl"

// locations index voxy.json "translucentDrawBuffers" (same as gbuffers_water)
layout(location = 0) out vec4 outColor;   // colortex0, alpha blended (voxy.json)
layout(location = 1) out vec4 outNormal;  // colortex6
layout(location = 2) out vec4 outReflect; // colortex7
#ifdef BLOOM
layout(location = 3) out vec4 outGlow;    // colortex2, replaced like gbuffers_water
#endif

// mirrors translucent.vsh/.fsh. LOD water has no texture glint
// (waterTexStrength = 0) and no SSR (reflectivity 0): LOD depth reaches the
// vanilla depth buffer only through Voxy's reprojection, not a surface the
// screen-space tracer can rely on
void voxy_emitFragment(VoxyFragmentParameters parameters) {
   float id = float(parameters.customId);
   vec3 feetPos = voxyFeetPos();
   float sunHeight = voxySunHeight();
   vec3 worldNormal = voxyWorldNormal(parameters.face);
   vec2 lightUV = parameters.lightMap;

   vec4 albedo  = parameters.sampledColour;
   vec4 color   = vec4(parameters.tinting.rgb, 1.0);
   vec4 ambient = getAmbientColor(lightUV.t, sunHeight);

   float fogMix = getFogMix(feetPos);
   vec3 gradientFogColor = getFogColor(fogMix, feetPos, sunHeight);

   if (id == 10008.0) { // water
      // KEEP IN SYNC with the water branch of translucent.fsh, glint at 0
      albedo.a = decodeWaterAlpha(albedo.a);
      albedo.rgb = vec3(WATER_BRIGHTNESS);
      color.ba = min(color.ba, vec2(max(color.r, color.g)*WATER_B, WATER_A));
   }

   ambient.rgb = contrastAmbient(ambient.rgb);
   ambient.rgb += voxyTorchColor(lightUV.s, ambient.rgb);

   albedo *= color * ambient;

   albedo.rgb = mix(albedo.rgb, gradientFogColor, fogMix);

   outColor   = albedo;
   outNormal  = vec4(0.5*worldNormal + 0.5, 1.0);
   outReflect = vec4(0.0, 0.0, 0.5, 1.0);
   #ifdef BLOOM
   outGlow    = vec4(0.0, 0.0, 0.0, albedo.a);
   #endif
}
