// colored light propagation volume (LPV), one propagation step per frame.
//
// the field holds light ENERGY per block cell. each step, every cell takes
// the average of itself and its NON-OPAQUE neighbors (slightly lossy),
// multiplied by how much light the cell lets through (0 for opaque blocks,
// the dye color for stained glass), plus what the cell emits:
//    next = emission + transmission * (sum_open + C*center) / (n_open + C + LOSS)
// so light flows around corners and through doorways but never through a
// wall, takes the color of the glass it crosses, and two sources add up.
// walls are not sinks: an opaque neighbor is left out of the average instead
// of counting as darkness, so a lit room does not darken toward its walls
// and a corridor carries light like vanilla does.
// a source's strength is its real vanilla light level (shadow.vsh).
//
// layout, cell encoding and constants: common/lpv.glsl.
//
// - ONE dispatch per frame, ping-ponging two persistent volumes by frame
//   parity (even frames write voxelLight, odd frames voxelLightAlt). every
//   cell is stamped with the frame number so readers know which is current.
// - the volume follows the camera block by block: last frame's field is read
//   at (cell + camera block delta); cells entering the volume start dark.
// - each work group copies its 8x4x8 block plus a one-cell halo of last
//   frame's field into shared memory (600 loads for 256 cells) and the
//   stencil reads that copy.

#include "/shader.h"

layout (local_size_x = 8, local_size_y = 4, local_size_z = 8) in;

// Iris's ComputeDirectiveParser reads this directive TEXTUALLY and cannot
// evaluate arithmetic ("ivec3(128 / 8, ...)" throws NumberFormatException and
// kills the dispatch) - literal integers only, one branch per volume size.
// values = (VOXEL_SIZE/8, VOXEL_HEIGHT/4, VOXEL_SIZE/8)
#if VOXEL_SIZE == 64
   const ivec3 workGroups = ivec3(8, 16, 8);
#endif
#if VOXEL_SIZE == 128
   const ivec3 workGroups = ivec3(16, 16, 16);
#endif
#if VOXEL_SIZE == 192
   const ivec3 workGroups = ivec3(24, 16, 24);
#endif
#if VOXEL_SIZE == 256
   const ivec3 workGroups = ivec3(32, 16, 32);
#endif
#if VOXEL_SIZE == 384
   const ivec3 workGroups = ivec3(48, 32, 48);
#endif
#if VOXEL_SIZE == 512
   const ivec3 workGroups = ivec3(64, 32, 64);
#endif

#ifdef COLORED_LIGHT_ACTIVE

   uniform int frameCounter;
   uniform vec3 cameraPosition;
   uniform vec3 previousCameraPosition;

   layout (rgba16f) uniform image3D voxelLight;
   layout (rgba16f) uniform image3D voxelLightAlt;
   layout (rgba8) uniform image3D voxelData;

   #include "/common/lpv.glsl"

   const ivec3 VOLUME = ivec3(VOXEL_SIZE, VOXEL_HEIGHT, VOXEL_SIZE); // = VOXEL_VOLUME_SIZE

   // work group block (must match local_size) and its block + halo
   const ivec3 BLOCK = ivec3(8, 4, 8);
   const ivec3 TILE  = BLOCK + 2;
   const int TILE_CELLS = TILE.x * TILE.y * TILE.z;           // 600
   const int GROUP_THREADS = BLOCK.x * BLOCK.y * BLOCK.z;     // 256

   shared vec3 tile[TILE_CELLS];      // last frame's field (re-indexed)
   shared float openTile[TILE_CELLS]; // this frame's map: 1 = light can enter

   int tileIndex(ivec3 t) {
      return t.x + TILE.x * (t.y + TILE.y * t.z);
   }

   vec3 readTile(ivec3 local, ivec3 offset) {
      return tile[tileIndex(local + 1 + offset)];
   }

   float readOpen(ivec3 local, ivec3 offset) {
      return openTile[tileIndex(local + 1 + offset)];
   }

   void main() {
      ivec3 p = ivec3(gl_GlobalInvocationID);
      ivec3 local = ivec3(gl_LocalInvocationID);
      float stamp = float(frameCounter % 2048);
      bool writeMain = frameCounter % 2 == 0;

      // the volumes are persistent and never cleared by Iris: on pack load
      // they hold arbitrary GPU memory (huge values, Inf, NaN). start dark
      // (uniform condition: the whole dispatch returns before the barrier)
      if (frameCounter < 2) {
         imageStore(voxelLight, p, vec4(0.0, 0.0, 0.0, stamp));
         imageStore(voxelLightAlt, p, vec4(0.0, 0.0, 0.0, stamp));
         return;
      }

      // last frame's field, re-indexed by the camera's whole-block movement
      ivec3 shift = ivec3(floor(cameraPosition) - floor(previousCameraPosition));
      ivec3 tileOrigin = ivec3(gl_WorkGroupID) * BLOCK - 1;

      for (int i = int(gl_LocalInvocationIndex); i < TILE_CELLS; i += GROUP_THREADS) {
         ivec3 t = ivec3(i % TILE.x, (i / TILE.x) % TILE.y, i / (TILE.x * TILE.y));
         ivec3 q = tileOrigin + t + shift; // last frame's index of this cell
         ivec3 c = tileOrigin + t;         // this frame's index
         vec3 previous = vec3(0.0);
         float open = 0.0;

         if (all(greaterThanEqual(q, ivec3(0))) && all(lessThan(q, VOLUME))) {
            previous = writeMain ? imageLoad(voxelLightAlt, q).rgb : imageLoad(voxelLight, q).rgb;
         }

         if (all(greaterThanEqual(c, ivec3(0))) && all(lessThan(c, VOLUME))) {
            open = imageLoad(voxelData, c).a < 0.75 ? 1.0 : 0.0;
         }

         // light only counts where it can be: a cell that is opaque now (or
         // outside the volume) contributes nothing, whatever it held before
         tile[i] = previous * open;
         openTile[i] = open;
      }

      barrier();

      vec3 gathered = readTile(local, ivec3( 1, 0, 0))
                    + readTile(local, ivec3(-1, 0, 0))
                    + readTile(local, ivec3( 0, 1, 0))
                    + readTile(local, ivec3( 0,-1, 0))
                    + readTile(local, ivec3( 0, 0, 1))
                    + readTile(local, ivec3( 0, 0,-1))
                    + readTile(local, ivec3( 0, 0, 0)) * LPV_CENTER;

      float openNeighbors = readOpen(local, ivec3( 1, 0, 0)) + readOpen(local, ivec3(-1, 0, 0))
                          + readOpen(local, ivec3( 0, 1, 0)) + readOpen(local, ivec3( 0,-1, 0))
                          + readOpen(local, ivec3( 0, 0, 1)) + readOpen(local, ivec3( 0, 0,-1));

      vec3 propagated = gathered / (openNeighbors + LPV_CENTER + LPV_LOSS);

      vec4 cell = imageLoad(voxelData, p);
      vec3 emission = cell.a <= 0.25 ? cell.rgb : vec3(0.0);
      vec3 transmission = cell.a <= 0.25 ? vec3(1.0)
                        : cell.a <  0.75 ? cell.rgb
                        : vec3(0.0);

      vec3 light = emission * emission * LPV_EMIT_SCALE + transmission * propagated;

      // NaN/Inf from any source dies here every frame (comparisons are false
      // for NaN, so it falls to 0); ceiling well inside rgba16f
      light = vec3(
         light.r >= 0.0 ? min(light.r, 30000.0) : 0.0,
         light.g >= 0.0 ? min(light.g, 30000.0) : 0.0,
         light.b >= 0.0 ? min(light.b, 30000.0) : 0.0
      );

      if (writeMain) {
         imageStore(voxelLight, p, vec4(light, stamp));
      } else {
         imageStore(voxelLightAlt, p, vec4(light, stamp));
      }
   }

#else

   void main() {}

#endif
