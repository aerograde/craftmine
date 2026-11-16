float getDiffuse(float sunHeight, float skyLight, bool isThin) {
        //  reduce under water
  return (isEyeInWater == 0 ? 1.0 : 0.5)
        //  reduce with fog
          * (1.0 - fogMix)
        //  reduce with rain strength
          * (1.0 - rainStrength)
        //  thin objects have constant diffuse
          * (isThin ? 0.75 : clamp(2.5*dot(normalize(gl_NormalMatrix * gl_Normal),
                                           normalize(shadowLightPosition)), -0.3333, 1.0))
        // no day/night fade here: shadows keep full geometric strength at all
        // times; the visible OPACITY is modulated per time of day at the
        // darkening step (DAY_SHADOW / NIGHT_SHADOW, textured_lit)
          ;
}