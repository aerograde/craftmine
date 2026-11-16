// emissive entity layers: spider/enderman/phantom eyes (vanilla EYES
// pipeline), warden/creaking emissive layers (ENTITY_TRANSLUCENT_EMISSIVE)
// and resource-pack emissive textures (ETF "_e") - Iris draws all of them
// with gbuffers_spidereyes

#include "/shader.h"

varying vec2 lightUV;
varying vec2 texUV;
varying vec4 color;

#ifdef BLOOM
   varying float bloomDistFade;

   #include "/common/math.glsl"
#endif

void main() {
   gl_Position = ftransform();

   texUV   = (gl_TextureMatrix[0] * gl_MultiTexCoord0).xy;
   lightUV = (gl_TextureMatrix[1] * gl_MultiTexCoord1).xy;
   color   = gl_Color;

   #ifdef BLOOM
      // same near-field cap as every other glow source
      bloomDistFade = 1.0 - rescale(gl_Position.w, 96.0, 144.0);
   #endif
}
