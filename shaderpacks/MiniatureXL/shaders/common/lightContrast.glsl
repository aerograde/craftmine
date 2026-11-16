// Light contrast setting: separates light levels a little more without
// changing the pack's look at 0. three levers, all neutral at 0:
//   - darker darks: dim ambient (caves, shade, night) is lowered, bright
//     ambient (sunlit surfaces) untouched
//   - block light pools: a steeper falloff with a brighter core, so light
//     sources read as pools instead of an even wash
//   - deeper sun shadows: the time-of-day shadow opacity is raised
// shared by gbuffers and the Voxy LOD patch (no uniforms here)

#ifndef LIGHT_CONTRAST_INCLUDED
#define LIGHT_CONTRAST_INCLUDED

vec3 contrastAmbient(vec3 ambient) {
   float level = clamp(1.6 * luma(ambient), 0.0, 1.0);
   return ambient * mix(1.0, level, 0.5 * LIGHT_CONTRAST);
}

float contrastTorchStrength(float strength) {
   return pow(strength, 1.0 + 0.5 * LIGHT_CONTRAST);
}

const float TORCH_CONTRAST_GAIN = 1.0 + 0.4 * LIGHT_CONTRAST;

float contrastShadowOpacity(float opacity) {
   return min(1.0, opacity * (1.0 + 0.6 * LIGHT_CONTRAST));
}

#endif
