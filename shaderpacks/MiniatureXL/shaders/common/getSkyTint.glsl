#ifndef SKY_TINT_INCLUDED
#define SKY_TINT_INCLUDED

// time-of-day sky tint (Sky colors settings screen). all-1.0 defaults leave
// the vanilla sky/fog colors untouched. the day/dusk/night windows match the
// sun-redness ramp in getLightColor.vsh so ground light and sky stay in sync
vec3 getSkyTint(float sunH) {
   float dayness   = clamp( 0.2*sunH - 3.929, 0.0, 1.0);
   float nightness = clamp(-0.2*sunH - 3.929, 0.0, 1.0);

   return SKY_DAY_TINT   * dayness
        + SKY_NIGHT_TINT * nightness
        + SKY_DUSK_TINT  * max(0.0, 1.0 - dayness - nightness);
}

#endif
