// view-space position, interpolated: the beam is one quad from the beacon
// up to the top of the world, so a distance taken per VERTEX is hundreds of
// blocks at both ends - interpolated, it read "too far to glow" even where
// the beam passes right in front of the player (beacon deep underground).
// the glow distance cap is measured per fragment instead (beaconbeam.fsh)
varying vec3 beamViewPos;
varying vec2 texUV;
varying vec4 color;

void main() {
   gl_Position = ftransform();

   texUV = (gl_TextureMatrix[0] * gl_MultiTexCoord0).st;
   color = gl_Color;

   beamViewPos = (gl_ModelViewMatrix * gl_Vertex).xyz;
}
