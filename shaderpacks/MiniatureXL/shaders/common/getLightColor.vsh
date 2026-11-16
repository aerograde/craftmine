vec3 getLightColor(float sunHeight, float skyLight) {
   float sunRedness = 1.0 - clamp(0.2*sunHeight - 3.929, 0.0, 1.0);

   // sun color blends between the configurable noon and dusk colors as the
   // sun approaches the horizon (Sky colors settings screen)
   vec3 lightColor = sunHeight > 0.01
      ? normalize(mix(SUN_NOON_COLOR, SUN_DUSK_COLOR, sunRedness) + vec3(0.001))
      : MOON_COLOR;

   // reduce color burn on dark spots
   return mix(vec3(luma(lightColor)), lightColor, skyLight);
}
