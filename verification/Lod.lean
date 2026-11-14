/-
  A model of the infinite-LOD ring arithmetic in
  `crates/horizon-client-render/src/lod.rs`.

  This checks the constants and the property that each coarser cell ring
  starts farther away than the previous one, through all 22 plan levels. It
  is a proof of the arithmetic model, not of the Rust renderer.

  Run: `lean verification/Lod.lean` (core Lean 4, no mathlib, nothing shipped).
-/

/-- The number of its own tile-widths a level splits at (`split_width`). -/
def splitWidth : Nat → Nat
  | 0 => 2
  | 1 => 8
  | 2 => 10
  | 3 => 6
  | 4 => 4
  | 5 => 3
  | _ => 2

/-- A level's footprint in blocks: `16 << level`. -/
def tileSize (level : Nat) : Nat := 16 * 2 ^ level

/-- The distance at which a level stops splitting, in blocks. -/
def ringBoundary (level : Nat) : Nat := tileSize level * splitWidth level

/-- The number of plan levels in `MAX_LEVEL`. -/
def maxLevel : Nat := 22

-- The near rings are the ones the picture depends on.
example : ringBoundary 1 = 256 := by decide
example : ringBoundary 2 = 640 := by decide
example : ringBoundary 3 = 768 := by decide
example : ringBoundary 4 = 1024 := by decide
example : ringBoundary 5 = 1536 := by decide

-- Every cell size the plan uses has a positive, strictly growing boundary.
theorem rings_grow :
    (List.range maxLevel).all (fun i => ringBoundary i < ringBoundary (i + 1)) = true := by
  decide

-- No level after the hand-tuned encoder gets a smaller boundary than the one
-- before it, so a visible cell cannot be replaced by a coarser one closer in.
theorem rings_grow_to_max : ∀ i, i < maxLevel → ringBoundary i < ringBoundary (i + 1) := by
  intro i h
  have hh := (List.all_eq_true.mp rings_grow) i (List.mem_range.mpr h)
  exact of_decide_eq_true hh

-- The far cells are at least as large as the near ones.
theorem cells_coarsen : ∀ i, i < maxLevel → 2 ^ i ≤ 2 ^ (i + 1) := by
  intro i _
  exact Nat.pow_le_pow_right (by decide) (Nat.le_succ i)
