#include "/program/voxy_common.glsl"

// locations index voxy.json "opaqueDrawBuffers": same targets, same order
// as textured_lit (DRAWBUFFERS:0672 / 067)
layout(location = 0) out vec4 outColor;   // colortex0
layout(location = 1) out vec4 outNormal;  // colortex6
layout(location = 2) out vec4 outReflect; // colortex7
#ifdef BLOOM
layout(location = 3) out vec4 outGlow;    // colortex2
#endif

// mirrors textured_lit.vsh/.fsh (GBUFFERS_TERRAIN), minus what LOD geometry
// does not carry: vertex AO, shadow-map lookups (LODs lie beyond the shadow
// distance), block reflections (SSR) and emissive glow
void voxy_emitFragment(VoxyFragmentParameters parameters) {
   float id = float(parameters.customId);
   vec3 feetPos = voxyFeetPos();
   float sunHeight = voxySunHeight();
   vec3 worldNormal = voxyWorldNormal(parameters.face);
   vec3 viewNormal = mat3(gbufferModelView) * worldNormal;
   vec2 lightUV = parameters.lightMap;
   float lightSourceLevel = voxyLightSourceLevel(id);

   bool isThin = id == 10031.0 || id == 10059.0 || id == 10175.0 || id == 10176.0
              || id == 10214.0 || id == 10226.0 || id == 10228.0
              || id == 10227.0 || id == 10253.0 || (id >= 10250.0 && id <= 10252.0); // KEEP IN SYNC: textured_lit.vsh

   vec4 albedo = parameters.sampledColour;
   vec4 color  = vec4(parameters.tinting.rgb, 1.0);

   // KEEP IN SYNC with the emissive texels of textured_lit.fsh
   float emissiveMask = getEmissiveMask(
      albedo.rgb * color.rgb / max(max(color.r, max(color.g, color.b)), 0.05),
      // copper bulbs share the copper block ids: LODs carry no block state,
      // so whether a bulb is lit is unknown - copper never self-lights here
      // trial spawners and vaults (10254/10255) only glow while active
      (id >= 20000.0 || id == 10254.0 || id == 10255.0) ? vec4(0.0)
                     : getEmissiveParams(int(parameters.customId)));

   #ifndef CL_LAVA_TEXTURE
      // KEEP IN SYNC with the vertex lava remap in textured_lit.vsh
      if (id == 10068.0) {
         color.rgb = mix(vec3(0.8, 0.5, 0.3), vec3(1.0), rescale(color.rgb, vec3(0.54), vec3(0.9)));
      }
   #endif

   #ifndef FLAT_LIGHTING
      vec3 lightNormal = isThin ? vec3(0.0, 1.0, 0.0) : worldNormal;

      color.rgb *= clamp(lightNormal.x * lightNormal.x * 0.6
                       + lightNormal.y * lightNormal.y * 0.25 * (3.0 + lightNormal.y)
                       + lightNormal.z * lightNormal.z * 0.8, lightSourceLevel, 1.0);
   #endif

   float fogMix = getFogMix(feetPos);
   fogMix = mix(fogMix, fogMix*fogMix, lightSourceLevel);
   vec3 gradientFogColor = getFogColor(fogMix, feetPos, sunHeight);

   #ifdef CL_LAVA_TEXTURE
      // KEEP IN SYNC with the lava recolor in textured_lit.fsh
      float tintChroma = max(color.r, max(color.g, color.b))
                       - min(color.r, min(color.g, color.b));

      if (id == 10068.0 && tintChroma > 0.06) {
         vec3 tintHue = color.rgb / max(max(color.r, max(color.g, color.b)), 0.05);
         float tintGain = min(1.0 / max(luma(tintHue), 0.05), 1.25);

         albedo.rgb = min(vec3(1.0), tintHue * (luma(albedo.rgb) * tintGain));
         color.rgb = vec3(1.0);
      }
   #endif

   vec4 ambient = getAmbientColor(lightUV.t, sunHeight);

   #ifdef THE_END
      ambient.rgb *= END_AMBIENT + 0.02*viewNormal;
   #endif

   #ifdef GLOWING_ORES
      float isOre = float(id == 10014.0 || id == 10220.0);

      ambient.rgb = mix(
         ambient.rgb,
         vec3(1.0, 0.9, 0.9),
         isOre * 0.3333*squaredLength(rescale(albedo.rgb, vec3(0.59), vec3(1.0)))
      );
   #endif

   albedo *= color;

   float albedoLuma = luma(albedo.rgb);

   #ifdef ENABLE_SHADOWS
      float skyLight = clamp(lightUV.t, 0.0, 1.0);
      float diffuse = voxyDiffuse(viewNormal, isThin, fogMix);
      vec3 lightColor = getLightColor(sunHeight, skyLight);
      float shadowOpacity = contrastShadowOpacity(mix(NIGHT_SHADOW, DAY_SHADOW, clamp(rescale(sunHeight, -3.0, 3.0), 0.0, 1.0)));

      // getLightStrength past the shadow distance: skylight-scaled diffuse
      float lightStrength = diffuse * rescale(lightUV.t, 0.3137, 0.6235);
      float lightBrightness = max(0.0, LIGHT_BRIGHTNESS - 0.5*pow3(albedoLuma));

      lightStrength = max(lightStrength, 0.75*lightSourceLevel);

      ambient.rgb *= mix(mix(vec3(1.0), SHADOW_COLOR, shadowOpacity), vec3(1.0), lightStrength);
      ambient.rgb *= 0.75 + (lightBrightness * lightStrength) * lightColor;
   #endif

   ambient.rgb = contrastAmbient(ambient.rgb);
   ambient.rgb += voxyTorchColor(lightUV.s, ambient.rgb);
   ambient.rgb = max(ambient.rgb, vec3(emissiveMask * emissiveHeat(albedo.rgb)));

   albedo *= ambient;

   #if defined THE_NETHER && defined NETHER_COLOR_GRADING
      albedo.rgb = mix(
         vec3(luma(albedo.rgb)),
         albedo.rgb,
         max(rescale(luma(ambient.rgb), 0.2, 0.6), rescale(albedoLuma, 0.15, 0.4))
      );
   #endif

   // gbuffers discard at fogMix > 0.999; Voxy discourages discard, so a
   // fully fogged LOD is drawn in the fog color instead
   albedo.rgb = mix(albedo.rgb, gradientFogColor, fogMix);

   outColor   = vec4(albedo.rgb, 1.0);
   outNormal  = vec4(vec3(0.0), 1.0);
   outReflect = vec4(vec3(0.0), 1.0);   // z != 0.5: no SSR on LOD terrain
   #ifdef BLOOM
   outGlow    = vec4(0.0, 0.0, 0.0, 1.0);
   #endif
}
