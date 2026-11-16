// colored light propagation volume (LPV): shared layout.
//
// the volume is a VOXEL_SIZE x VOXEL_HEIGHT x VOXEL_SIZE box of block cells
// centered on the camera's block, moving with it one whole block at a time
// (the compute re-indexes last frame's field by the camera's block delta).
// cell index = world block - lpvOrigin(cameraPosition).
//
// voxelData (rgba8, cleared every frame, written by shadow.vsh) per cell:
//   a <= 0.25  open    rgb = emitted light (palette color x level / 15)
//   a ~= 0.5   tinted  rgb = transmission color (stained glass, leaves)
//   a >= 0.75  opaque  rgb = 0 (light stops)
// cleared cells (0,0,0,0) are open and dark: air, and every shape that does
// not reach a block-grid corner.
//
// voxelLight / voxelLightAlt (rgba16f, ping-pong by frame parity):
//   rgb = light ENERGY (amplitude squared), a = frame stamp (frameCounter %
//   2048) so a reader can tell which of the two volumes is this frame's.

#ifndef LPV_INCLUDED
#define LPV_INCLUDED

vec3 lpvOrigin(vec3 cameraPos) {
   return floor(cameraPos) - floor(0.5 * VOXEL_VOLUME_SIZE);
}

bool lpvInside(vec3 cell) {
   return all(greaterThanEqual(cell, vec3(0.0))) && all(lessThan(cell, VOXEL_VOLUME_SIZE));
}

// emission energy for an rgba8 emission value: normalized so that a white
// level-15 source reads amplitude 1.0 one block away (steady state of the
// propagation in open space, see shadowcomp.csh)
const float LPV_EMIT_SCALE = 2.2085;

// propagation: next = emission + transmission * (sum of 6 neighbors
// + LPV_CENTER * center) / (6 + LPV_CENTER + LPV_LOSS)
const float LPV_CENTER = 1.0;
const float LPV_LOSS   = 0.1;

// reader transfer: intensity = 1 + ln(amplitude) / LPV_REACH, clamped. with
// the constants above and the default reach (4.0) a level-15 source reads
// 0.70 at 4 blocks, 0.45 at 8, 0.25 at 12 and 0 at ~17 - vanilla's linear
// falloff within a few percent, but round instead of diamond-shaped, and
// reaching a little past vanilla so the vanilla tail is colored too.
// a dimmer source (lower level, darker palette color) is proportionally
// dimmer and reaches less far
#define LPV_REACH (3.5 + CL_FALLOFF)

#endif
