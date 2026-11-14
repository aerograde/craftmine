# Verification

`Lod.lean` is a core-Lean model of the infinite-LOD ring arithmetic:

- `splitWidth` matches `split_width` in
  `crates/horizon-client-render/src/lod.rs`;
- `ringBoundary` computes the distance at which each cell size stops
  splitting;
- the proof checks the near-ring boundaries (256, 640, 768, 1024, 1536) and
  that boundaries strictly grow through all 22 plan levels.

Run it with the installed Lean 4 core toolchain:

```sh
lean verification/Lod.lean
```

It is a proof of the arithmetic model, not of the Rust/WGSL implementation,
and it is not part of the game or the shipped runtime. The renderer's own
`lod::tests::infinite_cells_grow_in_steps_from_the_resident_view` checks that
the actual plan never emits a cell larger than the ring at a sampled distance.
