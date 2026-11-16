//! The canonical sample lattice shared by every infinite-terrain level.
//!
//! Level `L` reads every `2^L`-th block from the same world-aligned grid. A
//! coarse point is therefore always a fine point, never an average or a
//! cell-centre choice, so refining detail inserts vertices without moving the
//! ones already on screen.

pub fn spacing(level: u8) -> i32 {
    1 << level
}

pub fn snap(value: i32, level: u8) -> i32 {
    value.div_euclid(spacing(level)) * spacing(level)
}

pub fn on_level(value: i32, level: u8) -> bool {
    value.rem_euclid(spacing(level)) == 0
}

pub fn is_sub_lattice(fine: u8, coarse: u8) -> bool {
    fine <= coarse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarser_levels_are_strict_sub_lattices() {
        for level in 0..=12 {
            let step = spacing(level);
            for value in -64..=64 {
                if on_level(value, level) {
                    assert!(on_level(value, level.saturating_sub(1)));
                }
                assert_eq!(snap(value, level) % step, 0);
            }
        }
    }

    #[test]
    fn snapping_never_moves_a_point_on_the_lattice() {
        for level in 0..=9 {
            let step = spacing(level);
            let first = -128i32.div_euclid(step) * step;
            for k in 0..=(256 / step) {
                let value = first + k * step;
                assert!(on_level(value, level));
                assert_eq!(snap(value, level), value);
            }
        }
    }

    #[test]
    fn a_shared_point_is_on_every_level() {
        let point = 16 * 512;
        for level in 0..=9 {
            assert!(on_level(point, level), "level {level}");
        }
        assert!(is_sub_lattice(0, 9));
    }

    /// The mesher draws a cell's centre as a fifth height (`CENTRE_SUBDIVIDE`,
    /// `mesh_heightfield`), which is only sound if that centre is a true sample
    /// of the finer level that tile splits into. It is: a cell is
    /// `size / CELLS` wide, so its centre is offset by half of that from the
    /// tile's lattice, and half a cell is exactly one spacing of the next
    /// level down. Refining a tile therefore inserts its centres and moves
    /// nothing already on screen — the promise this module exists to keep.
    #[test]
    fn a_cell_centre_is_the_next_finer_level_s_corner() {
        for level in 1..=9u8 {
            let size = 16i32 << level;
            let stride = size / 16;
            let half = stride / 2;
            assert!(
                half > 0,
                "level {level} has a {stride}-block cell, too narrow to halve"
            );
            assert!(
                on_level(half, level - 1),
                "level {level}'s cell centre (offset {half}) is not on level {}",
                level - 1
            );
        }
    }
}
