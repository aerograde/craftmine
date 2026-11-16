#include "/shader.h"

uniform sampler2D gtexture;

varying vec2 texUV;
varying float alpha;
varying float occluderOpacity;

// the colored light voxel map is written entirely by shadow.vsh: this stage
// only renders the shadow map itself
void main() {
   vec4 albedo = texture2D(gtexture, texUV);

   albedo.a *= alpha;

   if (albedo.a < 0.1) {
      discard;
   }

   // alpha = this occluder's shadow strength (entity fade, shadow.vsh); the
   // depth-tested write keeps the occluder nearest the light, the one that
   // actually casts the shadow
   gl_FragData[0] = vec4(albedo.rgb, occluderOpacity);
}
