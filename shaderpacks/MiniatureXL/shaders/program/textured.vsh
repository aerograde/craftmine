varying float bloomDist;
varying vec2 lightUV;
varying vec2 texUV;
varying vec4 color;

void main() {
   gl_Position = ftransform();

   texUV   = (gl_TextureMatrix[0] * gl_MultiTexCoord0).st;
   lightUV = (gl_TextureMatrix[1] * gl_MultiTexCoord1).st;
   color   = gl_Color;

   // clip-space depth: cannot be faked by custom matrices (Voxy LODs)
   bloomDist = max(gl_Position.w, length((gl_ModelViewMatrix * gl_Vertex).xyz));
}
