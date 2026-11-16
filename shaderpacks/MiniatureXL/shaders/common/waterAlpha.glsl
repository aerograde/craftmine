// water texture alpha, with resource-pack alpha markers decoded.
// Vanilla Tweaks "Wavy Water" ships water_still/water_flow at alpha 18/255
// (19 = its "clear water" variant): a marker its CORE shader reads and then
// multiplies the alpha back (x10 -> 0.706, x4.7368 -> 0.353). Core shaders
// do not run under a shader pack, so taken literally that water is 93%
// transparent - invisible. Photon ignores texture alpha for water; here
// the marker is decoded so vanilla and marked textures both keep their
// intended opacity. Any other water alpha passes through unchanged
float decodeWaterAlpha(float alpha) {
   float a255 = alpha * 255.0;

   if (a255 > 17.5 && a255 < 18.5) return alpha * 10.0;
   if (a255 > 18.5 && a255 < 19.5) return alpha * 4.7368;

   return alpha;
}
