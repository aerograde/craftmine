#define SSR_BINARY_Z_CUTOFF 16.0

float getReflectionVignette(vec2 uv) {
   uv.y = min(uv.y, 1.0 - uv.y);
   uv.x *= 1.0 - uv.x;
   uv.y *= uv.y;

   return 1.0 - pow(1.0 - uv.x, 50.0*uv.y);
}

// the ray is marched by its distance t along R. a point viewPos + t*R projects
// LINEARLY in homogeneous clip space (clip0 + t*clipR), so each step costs a
// 4-wide multiply-add and one divide instead of a full matrix product; and
// only the z and w rows of the inverse projection are needed to turn a depth
// sample back into view-space z (the x/y terms of those rows are zero for any
// perspective matrix, kept anyway so the result is exactly screen2view().z)
vec2 ssrClipToUV(vec4 clip) {
   return (clip.xy / clip.w) * 0.5 + 0.5;
}

float ssrSceneZ(vec2 uv, float depth, vec4 invRowZ, vec4 invRowW) {
   vec4 ndc = vec4(vec3(uv, depth) * 2.0 - 1.0, 1.0);
   return dot(invRowZ, ndc) / dot(invRowW, ndc);
}

vec4 getReflectionColor(float depth, vec3 normal, vec3 viewPos) {
   vec3 V = normalize(viewPos);
   vec3 R = normalize(reflect(V, normal));

   if (R.z >= -0.05) return vec4(0.0);

   float fresnel = 1.0 - dot(normal, -V);
   float grazingEpsilon = rescale(1.0 - abs(dot(R, normal)), 0.95, 1.0);
   float invR = 1.0 / abs(R.z);
   float invFar = 1.0 / (2.0*far);

   vec4 clip0 = gbufferProjection * vec4(viewPos, 1.0);
   vec4 clipR = gbufferProjection * vec4(R, 0.0);
   // rows (not columns) of the inverse projection that produce view z and w
   vec4 invRowZ = vec4(gbufferProjectionInverse[0].z, gbufferProjectionInverse[1].z,
                       gbufferProjectionInverse[2].z, gbufferProjectionInverse[3].z);
   vec4 invRowW = vec4(gbufferProjectionInverse[0].w, gbufferProjectionInverse[1].w,
                       gbufferProjectionInverse[2].w, gbufferProjectionInverse[3].w);

   float lengthR = 1.0;
   float oldT = 0.0;

   for (int i = 0; i < SSR_MAX_STEPS; i++) {
      float curT  = lengthR;
      float curZ  = viewPos.z + R.z * curT;
      vec2  curUV = ssrClipToUV(clip0 + clipR * curT);

      if (curUV.s < 0.0 || curUV.s > 1.0 || curUV.t < 0.0 || curUV.t > 1.0)
         break;

      float sceneDepth = texture2D(depthtex0, curUV).x;
      float sceneZ = ssrSceneZ(curUV, sceneDepth, invRowZ, invRowW);
      float distanceEpsilon = clamp(abs(sceneZ) * invFar, 0.0, 1.0);
      float epsilon = 1.0 + 0.1*max(distanceEpsilon, grazingEpsilon);
      float diffZ = curZ - sceneZ * epsilon;

      if (diffZ < 0.0) {
         float a = oldT;
         float b = curT;

         if (diffZ > -SSR_BINARY_Z_CUTOFF) {
            for (int j = 0; j < SSR_BINARY_STEPS; j++) {
               curT = (a + b) * 0.5;
               curZ = viewPos.z + R.z * curT;
               curUV = ssrClipToUV(clip0 + clipR * curT);
               sceneDepth = texture2D(depthtex0, curUV).x;
               sceneZ = ssrSceneZ(curUV, sceneDepth, invRowZ, invRowW);

               if (-curZ < -sceneZ) { a = curT; }
               else                 { b = curT; }
            }
         }

         return sceneDepth + 0.0001 <= depth
               ? vec4(0.0)
               : vec4(texture2D(colortex0, curUV).rgb,
                      getReflectionVignette(curUV) * fresnel);
      }

      oldT = curT;
      lengthR += max(SSR_STEP_SIZE * abs(diffZ) * invR, 1.0);
   }

   return vec4(0.0);
}