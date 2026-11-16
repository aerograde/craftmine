#extension GL_ARB_shader_image_load_store : enable

#include "/shader.h"

attribute vec4 mc_Entity;

varying vec2 texUV;
varying float alpha;
varying float occluderOpacity; // shadow strength of this occluder (shadow.fsh -> shadowcolor0.a)

#ifdef COLORED_LIGHT_ACTIVE
   uniform int entityId;
   uniform int blockEntityId;
   uniform int currentRenderedItemId;

   #ifdef CL_LAVA_TEXTURE
      uniform sampler2D gtexture;
   #endif

   // the whole voxel map is written from the VERTEX stage: every vertex of a
   // block knows its cell (at_midBlock), and the vertex stage runs whatever
   // the rasterizer later does with the primitive - edge-on from the sun,
   // back-facing (reversed culling), fully transparent overlays (the flame
   // on a burning mob) or below one shadow-map texel. nothing depends on the
   // shadow resolution or the sun angle
   layout (rgba8) uniform image3D voxelData;

   #include "/common/math.glsl"
   #include "/common/lightPalette.glsl"
   #include "/common/lpv.glsl"
#endif

#include "/common/getShadowDistortion.glsl"
#include "/common/waving.glsl"

// shared by the voxelizer, the waving offset and the entity shadow fade.
// AFTER waving.glsl: WAVING_ACTIVE is defined in there
#if defined COLORED_LIGHT_ACTIVE || defined WAVING_ACTIVE || defined ENABLE_SHADOWS
   attribute vec4 at_midBlock;

   uniform mat4 shadowModelViewInverse;
   uniform vec3 cameraPosition;
   uniform int renderStage;
#endif


#ifdef WAVING_ACTIVE
   uniform float frameTimeCounter;
   uniform float rainStrength;
#endif

void main() {
   #ifdef WAVING_ACTIVE
      // same waving offset as the gbuffers (common/waving.glsl): the shadow
      // of a swaying leaf or wave follows it. the colored light voxel map
      // below keeps using the block's real position
      vec4 wavingVertex = gl_Vertex;
      {
         vec3 wavingWorld = (shadowModelViewInverse * (gl_ModelViewMatrix * gl_Vertex)).xyz + cameraPosition;
         float wavingSky = (gl_TextureMatrix[1] * gl_MultiTexCoord1).t;

         #if defined IRIS_FEATURE_BLOCK_EMISSION_ATTRIBUTE || defined COLORED_LIGHT_ACTIVE
            bool wavingMid = dot(at_midBlock.xyz, at_midBlock.xyz) > 1.0
                          && all(lessThan(abs(at_midBlock.xyz), vec3(40.0)));
            float wavingMidY = at_midBlock.y;
         #else
            bool wavingMid = false;
            float wavingMidY = 0.0;
         #endif

         wavingVertex.xyz += getWaving(int(mc_Entity.x), wavingWorld, wavingMidY, wavingMid,
                                       wavingSky, frameTimeCounter, rainStrength);
      }

      gl_Position = gl_ModelViewProjectionMatrix * wavingVertex;
   #else
      gl_Position = ftransform();
   #endif

   gl_Position.xyz = getShadowDistortion(gl_Position.xyz);

   texUV = (gl_TextureMatrix[0] * gl_MultiTexCoord0).st;
   alpha = (mc_Entity.x == 10072.0 || mc_Entity.x == 10260.0) ? 0.0 : gl_Color.a; // fire, soul fire

   // entity shadow fade-in: Iris drops entities and block entities from the
   // shadow pass beyond a box of shadowDistance x entityShadowDistanceMul
   // around the camera (ShadowRenderer: BoxCuller), so their shadows popped
   // in. they fade out over the last 20% of that box instead: their opacity
   // goes to shadowcolor0.a (shadow.fsh) and the receivers scale their shadow
   // by it (getLightStrength.fsh). entities are what has no mid-block data
   // outside the terrain stages - Iris sets no render stage for entities in
   // the shadow pass, so they cannot be named directly
   occluderOpacity = 1.0;

   #ifdef ENABLE_SHADOWS
      if (entityShadowDistanceMul < 1.0) {
         bool hasMidBlock = dot(at_midBlock.xyz, at_midBlock.xyz) > 1.0
                         && all(lessThan(abs(at_midBlock.xyz), vec3(40.0)));

         #ifdef MC_RENDER_STAGE_TERRAIN_SOLID
            bool isTerrain = renderStage == MC_RENDER_STAGE_TERRAIN_SOLID
                          || renderStage == MC_RENDER_STAGE_TERRAIN_CUTOUT
                          || renderStage == MC_RENDER_STAGE_TERRAIN_CUTOUT_MIPPED
                          || renderStage == MC_RENDER_STAGE_TERRAIN_TRANSLUCENT;
         #else
            bool isTerrain = false;
         #endif

         if (!hasMidBlock && !isTerrain) {
            vec3 fromCamera = (shadowModelViewInverse * (gl_ModelViewMatrix * gl_Vertex)).xyz;
            float boxDistance = max(abs(fromCamera.x), max(abs(fromCamera.y), abs(fromCamera.z)));
            float entityRange = shadowDistance * entityShadowDistanceMul;

            occluderOpacity = 1.0 - clamp((boxDistance - 0.75 * entityRange) / (0.2 * entityRange), 0.0, 1.0);
         }
      }
   #endif

   #ifdef COLORED_LIGHT_ACTIVE
      vec3 worldPos = (shadowModelViewInverse * (gl_ModelViewMatrix * gl_Vertex)).xyz + cameraPosition;
      vec3 origin = lpvOrigin(cameraPosition);

      int id = int(mc_Entity.x);

      // ---- terrain: one cell per block ----------------------------------
      // Voxy and other mods also draw into the shadow pass with foreign
      // vertex formats: only real terrain stages, with a plausible mid-block
      // offset, may write the map
      #ifdef MC_RENDER_STAGE_TERRAIN_SOLID
         bool terrainStage = renderStage == MC_RENDER_STAGE_TERRAIN_SOLID
                          || renderStage == MC_RENDER_STAGE_TERRAIN_CUTOUT
                          || renderStage == MC_RENDER_STAGE_TERRAIN_CUTOUT_MIPPED
                          || renderStage == MC_RENDER_STAGE_TERRAIN_TRANSLUCENT;
      #else
         bool terrainStage = true;
      #endif

      bool midBlockKnown = dot(at_midBlock.xyz, at_midBlock.xyz) > 1.0
                        && all(lessThan(abs(at_midBlock.xyz), vec3(40.0)));

      // our own light-source ids are trusted without a mid-block offset:
      // fluid vertices (lava) may not carry one
      bool trustedEmitter = (id > 10200 && id < 10221) || id == 10068;

      bool plausibleTerrain = terrainStage && (midBlockKnown || trustedEmitter);

      if (plausibleTerrain) {
         // without a mid-block offset, step a hair inside the face instead
         vec3 worldNormal = mat3(shadowModelViewInverse) * (gl_NormalMatrix * gl_Normal);
         vec3 cell = (midBlockKnown ? floor(worldPos + at_midBlock.xyz / 64.0)
                                    : floor(worldPos - 0.01 * worldNormal)) - origin;

         // at_midBlock is cell center - vertex in 1/64 block: |32| on all
         // three axes = this vertex sits on a corner of the block cell
         bool gridCorner = midBlockKnown && all(greaterThan(abs(at_midBlock.xyz), vec3(31.5)));

         // vanilla emission of THIS block state (0-15): lit furnaces,
         // candle counts, sea pickles, respawn anchor charges... all exact.
         // nominal palette level only when the attribute is missing
         float level = at_midBlock.w > 0.5 && at_midBlock.w < 16.0 ? at_midBlock.w / 15.0
                     : midBlockKnown ? 0.0
                     : getLightPaletteLevel(id);

         // glowing materials (sculk, amethyst, brewing stand, powered
         // redstone...) emit at least their floor level, vanilla or not
         level = max(level, getLightEmitFloor(id));

         // water, glass, doors... (10230); cauldrons and anvils (20001);
         // campfires (in the torch/soul groups) and trial spawners/vaults
         // let light through even when unlit - vanilla filtered light 0
         bool open = id == 10008 || id == 10230 || id == 20001
                  || id == 10201 || id == 10202 || id == 10254 || id == 10255 || id == 10258;
         bool tinted = id >= 10231 && id <= 10247;           // stained glass, leaves

         vec4 value = vec4(-1.0);                            // -1: no write

         if (level > 0.0) {
            // emitters are open cells that emit: light leaves glowstone as
            // well as a torch, and a source never shadows itself
            vec3 color = getLightPaletteColor(id);

            #ifdef CL_LAVA_TEXTURE
               if (id == 10068 || id == 10208) {
                  // same rule as the surface recolor in textured_lit.fsh:
                  // a chromatic vertex tint (resource pack biome lava) IS the
                  // hue; with a neutral tint the texture carries it (one
                  // texel per vertex - the propagation averages the animated
                  // pattern over neighboring cells and frames)
                  float tintChroma = max(gl_Color.r, max(gl_Color.g, gl_Color.b))
                                   - min(gl_Color.r, min(gl_Color.g, gl_Color.b));
                  vec3 t = tintChroma > 0.06
                     ? gl_Color.rgb
                     : texture2D(gtexture, texUV).rgb * gl_Color.rgb;

                  color = t / max(max(t.r, max(t.g, t.b)), 0.05);
               }
            #endif

            value = vec4(color * level, 0.0);
         } else if (open) {
            // nothing to write: a cleared cell is already open and dark
         } else if (tinted) {
            if (gridCorner) value = vec4(getLightTint(id), 0.5);
         } else if (gridCorner) {
            value = vec4(0.0, 0.0, 0.0, 1.0);
         }

         if (value.a >= 0.0 && lpvInside(cell)) {
            imageStore(voxelData, ivec3(cell), value);
         }
      }

      // ---- entity-carried lights: emitters in the cell they occupy -------
      int lightId = 0;
      float lightLevel = 0.0;

      // block entities (ender chest and its animating lid...): the id
      // arrives via blockEntityId, not mc_Entity
      if (blockEntityId > 10200 && blockEntityId < 10213) {
         lightId = blockEntityId;
         lightLevel = getLightPaletteLevel(lightId);
      }

      #if defined CL_ENTITY_LIGHT || defined CL_ITEM_LIGHT
         // dropped items & item frames (CL_ITEM_LIGHT), items held by other
         // players and mobs - and the shadow-pass player model itself, which
         // carries the player's own held light (CL_ENTITY_LIGHT)
         if (currentRenderedItemId >= 10000 && currentRenderedItemId < 20000) {
            bool droppedOrFramed = entityId == 10301 || entityId == 10303;

            #ifdef CL_ITEM_LIGHT
               bool carriedOk = droppedOrFramed;
            #else
               bool carriedOk = false;
            #endif
            #ifdef CL_ENTITY_LIGHT
               carriedOk = carriedOk || !droppedOrFramed;
            #endif

            if (carriedOk) {
               lightId = currentRenderedItemId;
               lightLevel = 0.75 * getLightPaletteLevel(lightId);
            }
         }
      #endif

      #ifdef CL_ENTITY_LIGHT
         // burning entities & flaming arrows (Iris hardcoded flame-overlay
         // id), plus intrinsically glowing mobs
         if (entityId == 10302 || entityId == 10304 || entityId == 10305) {
            lightId    = entityId == 10302 ? 10201   // fire -> flame color
                       : entityId == 10304 ? 10208   // blaze, magma cube -> lava color
                       : 10207;                      // glow squid, allay -> glow color
            lightLevel = entityId == 10302 ? 0.8
                       : entityId == 10304 ? 0.65
                       : 0.4;
         }
      #endif

      if (lightLevel > 0.0 && !plausibleTerrain) {
         vec3 cell = floor(worldPos) - origin;

         if (lpvInside(cell)) {
            imageStore(voxelData, ivec3(cell), vec4(getLightPaletteColor(lightId) * lightLevel, 0.0));
         }
      }

      #ifndef ENABLE_SHADOWS
         // no shadow map is ever read here (Nether, End, or Overworld with
         // shadows off): this pass exists only for the voxel writes above,
         // done in the vertex stage. send every primitive outside the clip
         // volume (z > w) so nothing is rasterized: no shadow-map fill, no
         // fragment work - at shadowMapResolution 1024 that was a full
         // depth+color render of the world around the player per frame
         gl_Position = vec4(0.0, 0.0, 2.0, 1.0);
      #endif
   #endif
}
