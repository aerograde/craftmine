#include "/shader.h"

attribute vec4 mc_Entity;

uniform float frameTimeCounter;
uniform float rainStrength;
uniform float screenBrightness;
uniform int isEyeInWater;
uniform int worldTime;
uniform sampler2D lightmap;
uniform vec3 sunPosition;

varying float fogMix;
varying float reflectivity;
varying float waterTexStrength;
varying vec2 lightUV;
varying vec2 texUV;
varying vec3 feetPos;
varying vec3 gradientFogColor;
varying vec3 normal;
varying vec4 ambient;
varying vec4 color;

#ifdef BLOOM
   #define BLOOM_PASS
#endif

#ifdef BLOOM_PASS
   varying float bloomDistFade;

   #ifdef IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE
      attribute vec4 at_midBlock;
   #endif

   flat varying float lightSourceLevel;
   flat varying float glowScale; // Glow per block slider (nether portal...)
   flat varying vec4 emissive;   // emissive model (lightPalette.glsl), 0 = none
   flat varying float portalBlob; // 1 on an emitting nether portal (halo kept on its surface)
#endif

#ifdef COLORED_LIGHT_ACTIVE
   varying vec3 lpvNormal; // geometric world-space normal (colored light lookup)
#endif

flat varying float portalSurface; // 1 on the nether portal (opacity, translucent.fsh)

#include "/common/math.glsl"
#include "/common/transformations.glsl"
#include "/common/getFogMix.vsh"
#include "/common/lightContrast.glsl"
#include "/common/lightPalette.glsl"
#include "/common/getFogColor.vsh"
#include "/common/getAmbientColor.glsl"
#include "/common/getViewPosition.vsh"
#include "/common/waving.glsl"
#include "/common/getWaterTextureStrength.vsh"
#include "/common/getWaterWave.vsh"

void main() {
   gl_Position = ftransform();

   #ifdef WAVY_WATER
      if (mc_Entity.x == 10008.0) {
         // water surface waves (common/waving.glsl), same as shadow.vsh
         vec3 wave = getWaving(10008, view2feet((gl_ModelViewMatrix * gl_Vertex).xyz) + cameraPosition,
                               0.0, false, (gl_TextureMatrix[1] * gl_MultiTexCoord1).t,
                               frameTimeCounter, rainStrength);

         if (dot(wave, wave) > 0.0) {
            gl_Position = gl_ModelViewProjectionMatrix * vec4(gl_Vertex.xyz + wave, 1.0);
         }
      }
   #endif

   float sunHeight = view2feet(sunPosition).y;

   portalSurface = float(mc_Entity.x == 10206.0);

   color        = gl_Color;
   texUV        = (gl_TextureMatrix[0] * gl_MultiTexCoord0).st;
   lightUV      = (gl_TextureMatrix[1] * gl_MultiTexCoord1).st;
   normal       = gl_Normal;
   ambient      = getAmbientColor(lightUV.t, sunHeight);
   reflectivity = GLASS_REFLECTIVITY;

   feetPos = view2feet(getViewPosition());

   #ifdef COLORED_LIGHT_ACTIVE
      lpvNormal = mat3(gbufferModelViewInverse) * normalize(gl_NormalMatrix * gl_Normal);
   #endif
   fogMix = getFogMix(feetPos);
   gradientFogColor = getFogColor(fogMix, feetPos, sunHeight);

   ambient.rgb *= color.a;
   ambient.rgb = contrastAmbient(ambient.rgb);
   color.a = 1.0;

   #ifdef BLOOM_PASS
      // clip-space depth cap: unfakeable, see textured_lit.vsh
      bloomDistFade = 1.0 - rescale(max(gl_Position.w, length(feetPos)), 96.0, 144.0);

      #ifdef IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE
         lightSourceLevel = at_midBlock.w < 16.0 ? at_midBlock.w / 15.0 : 0.0;

         // reject implausible mid-block data (Voxy LOD water/translucents)
         if (dot(at_midBlock.xyz, at_midBlock.xyz) <= 1.0
          || any(greaterThan(abs(at_midBlock.xyz), vec3(40.0)))) {
            lightSourceLevel = 0.0;
         }
      #else
         lightSourceLevel = 0.0;
      #endif

      glowScale = getGlowScale(int(mc_Entity.x), lightSourceLevel > 0.12);

      // translucent emitters glow on their emissive texels like terrain
      // (the luma test missed the dark violet of the nether portal), and
      // only while they emit
      // the nether portal is the only translucent of group 10206 (crying
      // obsidian and respawn anchors are terrain): its own lightness range
      bool isPortal = mc_Entity.x == 10206.0;

      emissive = lightSourceLevel <= 0.12 ? vec4(0.0)
               : isPortal ? PORTAL_EMISSIVE
               : getEmissiveParams(int(mc_Entity.x));

      portalBlob = float(isPortal && emissive.w > 0.0);
   #endif

   if (mc_Entity.x == 10008.0) {
      float posRandom = random(floor(feetPos.xz) + floor(cameraPosition.xz));

      #if WATER_WAVE_SIZE > 0

         if (abs(normal.y) > 0.8) {
            normal = normalize(normal + getWaterWave(posRandom, feetPos));
         }

      #endif

      reflectivity = WATER_REFLECTIVITY;
      waterTexStrength = getWaterTextureStrength(posRandom) * waterEdgeFade(feetPos, far);
   }

   normal = ndc2screen(normal);
}