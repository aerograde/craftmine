// waving foliage and water (Vanilla Tweaks "Wavy Leaves / Plants / Water"
// rebuilt in the pack - a resource pack's core shaders do not run under a
// shader pack). One world-space offset per vertex, added in the gbuffers
// AND the shadow pass so shadows sway with what casts them.
//
// continuity: the offset is a function of the vertex's WORLD position only
// (plus a per-category weight that is equal on shared vertices), so stacked
// vines, double plants and neighbouring water blocks never crack apart.
// world coordinates are wrapped to 256 blocks and every spatial frequency
// is an integer multiple of 2pi/256: the wave is identical across the wrap
// (no seam) and float precision stays fine at any distance from 0,0.
//
// indoor logic: the wind follows the vertex's SKY light - full under open
// sky, fading out under a roof (Indoor waving sets what is left indoors);
// rain strengthens it.

#if defined WAVY_LEAVES || defined WAVY_PLANTS || defined WAVY_WATER
   #define WAVING_ACTIVE
#endif

#ifdef WAVING_ACTIVE

#define WAVE_K (6.2831853 / 256.0)

// two-axis sway, -1..1-ish
vec2 waveSway(vec3 q, float t) {
   return vec2(
      0.6 * sin(t * 1.40 + 37.0 * q.x + 23.0 * q.z + 11.0 * q.y)
    + 0.3 * sin(t * 2.30 + 61.0 * q.x - 17.0 * q.z + 1.3)
    + 0.1 * sin(t * 4.10 + 97.0 * q.x + 71.0 * q.z),
      0.6 * sin(t * 1.10 + 29.0 * q.z - 31.0 * q.x + 13.0 * q.y + 2.0)
    + 0.3 * sin(t * 2.70 + 53.0 * q.z + 19.0 * q.x + 0.7)
    + 0.1 * sin(t * 3.70 + 89.0 * q.z - 43.0 * q.x)
   );
}

// skyLight: 0..1 (lightmap sky coordinate), midBlockY: at_midBlock.y
// (block center - vertex, 1/64 block; < 0 above the center), hasMidBlock:
// whether midBlockY is real (plants need it to know their base)
vec3 getWaving(int id, vec3 worldPos, float midBlockY, bool hasMidBlock,
               float skyLight, float time, float rain) {
   float wind = mix(WAVE_INDOORS, 1.0, smoothstep(0.55, 0.9, skyLight))
              * WAVE_STRENGTH * (1.0 + 0.7 * rain);

   if (wind <= 0.0) return vec3(0.0);

   vec3 q = mod(worldPos, 256.0) * WAVE_K;
   float t = time * WAVE_SPEED;

   #ifdef WAVY_LEAVES
      if (id == 10247) {
         // leaves: the whole block rustles, small
         vec2 s = waveSway(q, t);
         return 0.045 * wind * vec3(s.x, 0.4 * s.y, s.y);
      }
   #endif

   #ifdef WAVY_PLANTS
      if (id == 10250 || id == 10227) {
         // hanging and stacked plants (vines, roots, kelp): sway as a
         // whole, gently, so the strand stays continuous
         vec2 s = waveSway(q, t);
         return 0.06 * wind * vec3(s.x, 0.0, s.y);
      }

      if (hasMidBlock && (id == 10059 || id == 10031 || id == 10226 || id == 10251 || id == 10253)) {
         // ground plants bend from their base: weight 0 at the bottom of
         // the block, 1 at its top; upper halves of double plants continue
         // from 1 to 2 (their base is the lower half's top)
         float top = clamp(0.5 - midBlockY / 64.0, 0.0, 1.0);
         float weight = id == 10251 ? 1.0 + top : top;

         if (weight <= 0.0) return vec3(0.0);

         vec2 s = waveSway(q, t);
         return 0.11 * wind * weight * vec3(s.x, -0.15 * abs(s.x + s.y), s.y);
      }
   #endif

   #ifdef WAVY_WATER
      if (id == 10008 && fract(worldPos.y) > 0.02) {
         // water: the surface (any vertex above its block's floor) rises
         // and falls; the floor edge stays put so no gap opens below
         float h = 0.55 * sin(t * 1.9 + 41.0 * q.x + 23.0 * q.z)
                 + 0.30 * sin(t * 2.6 - 27.0 * q.x + 53.0 * q.z + 1.7)
                 + 0.15 * sin(t * 3.9 + 83.0 * q.x - 61.0 * q.z);
         return vec3(0.0, 0.05 * wind * h - 0.03 * wind, 0.0);
      }
   #endif

   return vec3(0.0);
}

#endif
