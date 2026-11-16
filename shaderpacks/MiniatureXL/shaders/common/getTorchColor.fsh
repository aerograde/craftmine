#include "/common/lightContrast.glsl"

#ifdef COLORED_LIGHT_ACTIVE
   uniform sampler3D voxelLightSampler;
   uniform sampler3D voxelLightAltSampler;
   uniform int frameCounter;

   #include "/common/lpv.glsl"
#endif

#ifdef COLORED_LIGHT
   uniform int heldItemId;
   uniform int heldItemId2;

   #include "/common/lightPalette.glsl"
#endif

// strength of the light-volume light at the last getTorchColor() call:
// lets callers tell a face lit by a real source from a fullbright overlay
float lpvStrengthOut = 0.0;

// block light. two sources of truth, combined per surface:
//   - vanilla: the lightmap level (torchLight), plus the shader hand light.
//     intensity only, no color; also carries light the volume cannot see
//     (LambDynamicLights, sources outside the volume)
//   - colored light (COLORED_LIGHT_ACTIVE): the propagation volume, sampled
//     half a block in front of the surface (the air cell the face looks
//     into). its intensity comes from the real emission level and distance
//     of every source, its hue from their palette colors mixed in proportion
//     to their contribution, and it never crosses an opaque block
// the brighter of the two sets the strength; the volume's share of it sets
// how much of its hue replaces the default warm gradient
vec3 getTorchColor(float torchLight, vec3 ambient, vec3 feetPos, vec3 worldNormal) {
   float strength = rescale(torchLight, TORCH_UV_SCALE.x, TORCH_UV_SCALE.y);
   float handLight = 0.0;

   #if HAND_DYNAMIC_LIGHTING >= 0
      handLight = rescale(float(heldBlockLightValue) - SQRT_2 * length(feetPos), 0.0, 15.0);
      strength = max(strength, handLight);
   #endif

   float lpvShare = 0.0; // fraction of the strength the volume accounts for
   vec3 lpvLight = vec3(0.0);

   #ifdef COLORED_LIGHT_ACTIVE
      vec3 cellCoord = feet2world(feetPos) + 0.5 * worldNormal - lpvOrigin(cameraPosition);
      vec3 uvw = cellCoord / VOXEL_VOLUME_SIZE;

      // fade to vanilla over the outer fifth of the volume and past the
      // range setting
      vec3 edge = abs(2.0 * uvw - 1.0);
      float fade = (1.0 - rescale(max(edge.x, max(edge.y, edge.z)), 0.8, 1.0))
                 * (1.0 - rescale(length(feetPos.xz) / CL_RANGE_EFF, 0.8, 1.0));

      if (fade > 0.0) {
         // this frame's volume: the one stamped with this frame's number
         // (the compute writes voxelLight on even frames). falls back to the
         // other if the compute did not run this frame
         float stamp = mod(float(frameCounter), 2048.0);
         bool mainFirst = mod(float(frameCounter), 2.0) < 0.5;

         vec4 field = mainFirst ? texture3D(voxelLightSampler, uvw) : texture3D(voxelLightAltSampler, uvw);

         if (abs(field.a - stamp) > 0.5) {
            field = mainFirst ? texture3D(voxelLightAltSampler, uvw) : texture3D(voxelLightSampler, uvw);
         }

         // energy -> amplitude; NaN/negative rejected by comparison
         vec3 amplitude = sqrt(max(field.rgb, vec3(0.0)));
         float peak = max(amplitude.r, max(amplitude.g, amplitude.b));

         if (peak > 1.0e-5 && peak < 1.0e4) {
            // log transfer (see common/lpv.glsl): a level-15 white source
            // reads 1.0 one block away and follows vanilla's falloff
            float lpvStrength = clamp(1.0 + log(peak) / LPV_REACH, 0.0, 1.0) * fade;

            // vanilla stays a loose ceiling: where the lightmap is dark
            // (blocks outside the shadow pass) the volume cannot add more
            // than half - which still leaves room for the materials that
            // glow without vanilla light (sculk, amethyst, redstone...)
            lpvStrength = min(lpvStrength, strength + 0.5);

            // hue: wherever the volume carries a real part of the light it
            // colors ALL of it - vanilla light beyond the volume's falloff
            // comes from the same sources, and leaving it warm would draw a
            // warm ring around every colored light. only light the volume
            // does not see at all keeps the default gradient
            lpvShare = rescale(lpvStrength / max(strength, 1.0e-4), 0.0, 0.35);
            lpvStrengthOut = lpvStrength;
            strength = max(strength, lpvStrength);
            lpvLight = amplitude;
         }
      }
   #endif

   strength = mix(strength*strength, smoothe(strength), screenBrightness);
   strength = mix(strength*strength, strength, max(1.0 - screenBrightness, eyeBrightnessSmooth.y/240.0));

   // block light softness: an exponent below 1 lifts the low end of the
   // light curve, softening dark corners
   strength = pow(strength, 1.0 / TORCH_FALLOFF);
   strength = contrastTorchStrength(strength);

   vec3 torchColor = mix(
      mix(TORCH_OUTER_COLOR, TORCH_MIDDLE_COLOR, strength),
      TORCH_INNER_COLOR,
      slopeTo1(strength, 8.0)
   );

   #ifdef COLORED_LIGHT
      // held-light hue: light the volume does not carry (LambDynamicLights
      // through the vanilla lightmap, the shader hand light) takes the color
      // of the item in hand close to the player. checks both hands; reach
      // scales with the item's nominal light level
      int handId = heldItemId >= 10000 && heldItemId < 20000 ? heldItemId
                 : heldItemId2 >= 10000 && heldItemId2 < 20000 ? heldItemId2 : 0;

      if (handId != 0) {
         float handReach = 15.0 * getLightPaletteLevel(handId);
         float handMix = 1.0 - rescale(SQRT_2 * length(feetPos), 0.3 * handReach, handReach);

         vec3 handPalette = getLightPaletteColor(handId);
         vec3 handHue = handPalette * (luma(torchColor) / max(luma(handPalette), 0.001));

         torchColor = mix(torchColor, handHue, smoothe(handMix) * (1.0 - lpvShare));
      }
   #endif

   #ifdef COLORED_LIGHT_ACTIVE
      // the volume's hue at the gradient's luminance: strength alone sets
      // the brightness, the sources' mixed colors set the hue
      vec3 lpvHue = lpvLight * (luma(torchColor) / max(luma(lpvLight), 1.0e-5));

      torchColor = mix(torchColor, lpvHue, lpvShare);
   #endif

   #ifdef COLORED_LIGHT
      // the strength slider deepens the color's saturation. above 1.0 the mix
      // EXTRAPOLATES past the color, then the result is RESCALED back to the
      // color's original peak instead of clipping: purer hue, same brightness
      float torchPeak = max(torchColor.r, max(torchColor.g, torchColor.b));

      torchColor = max(vec3(0.0), mix(
         vec3(luma(torchColor)),
         torchColor,
         0.5 + 0.5*COLORED_LIGHT_STRENGTH
      ));

      torchColor *= torchPeak / max(max(torchColor.r, max(torchColor.g, torchColor.b)), 1.0e-4);
   #endif

   return max(0.0, 1.0 - luma(ambient)) * strength * TORCH_CONTRAST_GAIN * torchColor;
}
