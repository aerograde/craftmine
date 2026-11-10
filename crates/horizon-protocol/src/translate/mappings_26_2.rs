//! The 26.2/26.3 registry-id shifts, decoded from ViaVersion's
//! `mappings-26.2to26.3.nbt` (the `SHIFTS_ID` strategy). Each pair is
//! `(first 26.2 id, its 26.3 id)`; ids advance together until the next
//! pair. A 26.3 id with no 26.2 source is absent from the inverse.

/// One registry's shift table.
pub struct ShiftMap {
    pub size: i32,
    pub mapped: i32,
    pub pairs: &'static [(i32, i32)],
}

pub const BLOCK_STATES: ShiftMap = ShiftMap {
    size: 32366,
    mapped: 35723,
    pairs: &[
        (27, 28),
        (85, 88),
        (163, 169),
        (198, 207),
        (225, 237),
        (252, 267),
        (504, 603),
        (2187, 2302),
        (2252, 2368),
        (2309, 3801),
        (3240, 4796),
        (5623, 7211),
        (5899, 7495),
        (6611, 8271),
        (6747, 8415),
        (6879, 8549),
        (7690, 9424),
        (9493, 11235),
        (10639, 12382),
        (10891, 12658),
        (12372, 14219),
        (13384, 15237),
        (13402, 15322),
        (13408, 15255),
        (13474, 16688),
        (13480, 15321),
        (13481, 15328),
        (13740, 15619),
        (14028, 15939),
        (14572, 16547),
        (14713, 16694),
        (15046, 18403),
    ],
};

pub const BLOCKS: ShiftMap = ShiftMap {
    size: 1196,
    mapped: 1286,
    pairs: &[
        (23, 24),
        (34, 36),
        (58, 61),
        (70, 74),
        (79, 84),
        (88, 94),
        (97, 106),
        (126, 136),
        (134, 145),
        (156, 199),
        (190, 234),
        (219, 264),
        (233, 279),
        (245, 292),
        (255, 303),
        (270, 319),
        (325, 375),
        (397, 448),
        (421, 473),
        (452, 505),
        (521, 575),
        (608, 663),
        (611, 678),
        (612, 666),
        (623, 716),
        (624, 677),
        (625, 679),
        (636, 691),
        (645, 701),
        (654, 711),
        (659, 717),
        (726, 816),
    ],
};

pub const ITEMS: ShiftMap = ShiftMap {
    size: 1537,
    mapped: 1658,
    pairs: &[
        (72, 73),
        (85, 87),
        (170, 173),
        (184, 188),
        (195, 200),
        (207, 213),
        (218, 227),
        (232, 242),
        (256, 298),
        (277, 320),
        (307, 351),
        (341, 386),
        (381, 427),
        (478, 525),
        (658, 737),
        (788, 868),
        (804, 885),
        (817, 899),
        (838, 921),
        (858, 942),
        (909, 995),
        (1025, 1112),
        (1037, 1125),
        (1082, 1186),
        (1131, 1236),
        (1134, 1255),
    ],
};

pub const ENTITIES: ShiftMap = ShiftMap {
    size: 158,
    mapped: 161,
    pairs: &[(33, 34), (105, 108)],
};

pub const PARTICLES: ShiftMap = ShiftMap {
    size: 125,
    mapped: 128,
    pairs: &[(43, 46)],
};

pub const SOUNDS: ShiftMap = ShiftMap {
    size: 1968,
    mapped: 1991,
    pairs: &[(492, 496)],
};

pub const COMPONENTS: ShiftMap = ShiftMap {
    size: 111,
    mapped: 122,
    pairs: &[(40, -1), (41, 42), (42, 45), (45, -1), (46, 48), (82, 88)],
};

/// A 26.2 id as its 26.3 id, or `None` when 26.3 removed it.
pub fn to_26_3(map: &ShiftMap, id: i32) -> Option<i32> {
    if !(0..map.size).contains(&id) {
        return None;
    }
    let i = match map.pairs.binary_search_by_key(&id, |&(at, _)| at) {
        Ok(i) => i,
        Err(0) => return Some(id),
        Err(i) => i - 1,
    };
    let (at, to) = map.pairs[i];
    (to >= 0).then(|| to + (id - at))
}

/// A 26.3 id as its 26.2 id, or `None` when 26.3 introduced it.
pub fn to_26_2(map: &ShiftMap, id: i32) -> Option<i32> {
    if !(0..map.mapped).contains(&id) {
        return None;
    }
    for (i, &(at, to)) in map.pairs.iter().enumerate() {
        let end = map.pairs.get(i + 1).map_or(map.size, |&(next, _)| next);
        if to < 0 {
            continue;
        }
        if id >= to && id < to + (end - at) {
            return Some(at + (id - to));
        }
    }
    // Before the first shift the ids are equal.
    let first = map.pairs.first().map_or(map.size, |&(at, _)| at);
    (id < first).then_some(id)
}

/// A 26.3 block state as 26.2's, with stone for a state 26.2 lacks.
pub fn block_state_to_26_2(id: i32) -> i32 {
    to_26_2(&BLOCK_STATES, id).unwrap_or(1)
}

/// A 26.2 block state as 26.3's, with stone for a state 26.3 lacks.
pub fn block_state_to_26_3(id: i32) -> i32 {
    to_26_3(&BLOCK_STATES, id).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entity_insertions_are_at_33_and_105() {
        assert_eq!(to_26_2(&ENTITIES, 34), Some(33));
        assert_eq!(to_26_3(&ENTITIES, 33), Some(34));
        assert_eq!(to_26_2(&ENTITIES, 33), None); // 26.3-only
        assert_eq!(to_26_2(&ENTITIES, 108), Some(105));
        assert_eq!(to_26_3(&ENTITIES, 105), Some(108));
        assert_eq!(to_26_2(&ENTITIES, 160), Some(157));
    }

    #[test]
    fn every_id_round_trips_through_every_table() {
        /// Enough examples to see the pattern without burying it.
        const EXAMPLES: usize = 5;

        let tables: [(&str, &ShiftMap); 7] = [
            ("BLOCK_STATES", &BLOCK_STATES),
            ("BLOCKS", &BLOCKS),
            ("ITEMS", &ITEMS),
            ("ENTITIES", &ENTITIES),
            ("PARTICLES", &PARTICLES),
            ("SOUNDS", &SOUNDS),
            ("COMPONENTS", &COMPONENTS),
        ];
        let mut failures: Vec<String> = Vec::new();
        let mut failed = 0usize;

        for (name, map) in tables {
            // 26.2 -> 26.3 -> 26.2: an id 26.3 kept must come back unchanged.
            for id in 0..map.size {
                let Some(mapped) = to_26_3(map, id) else {
                    continue;
                };
                let back = to_26_2(map, mapped);
                if back != Some(id) {
                    failed += 1;
                    if failures.len() < EXAMPLES {
                        failures.push(format!(
                            "{name}: 26.2 {id} -> 26.3 {mapped} -> 26.2 {back:?}, want {id}"
                        ));
                    }
                }
            }

            // 26.3 -> 26.2 -> 26.3: an id 26.2 had must come back unchanged,
            // which also catches a 26.3 id two 26.2 ids claim.
            for mapped in 0..map.mapped {
                let Some(old) = to_26_2(map, mapped) else {
                    continue;
                };
                let back = to_26_3(map, old);
                if back != Some(mapped) {
                    failed += 1;
                    if failures.len() < EXAMPLES {
                        failures.push(format!(
                            "{name}: 26.3 {mapped} -> 26.2 {old} -> 26.3 {back:?}, want {mapped}"
                        ));
                    }
                }
            }
        }

        assert!(
            failed == 0,
            "{failed} round-trip failures, first {}:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn the_tables_have_their_official_sizes() {
        // (26.2, 26.3): the registry lengths the mappings were decoded from.
        assert_eq!((BLOCK_STATES.size, BLOCK_STATES.mapped), (32366, 35723));
        assert_eq!((BLOCKS.size, BLOCKS.mapped), (1196, 1286));
        assert_eq!((ITEMS.size, ITEMS.mapped), (1537, 1658));
        assert_eq!((ENTITIES.size, ENTITIES.mapped), (158, 161));
        assert_eq!((PARTICLES.size, PARTICLES.mapped), (125, 128));
        assert_eq!((SOUNDS.size, SOUNDS.mapped), (1968, 1991));
        assert_eq!((COMPONENTS.size, COMPONENTS.mapped), (111, 122));
    }

    #[test]
    fn block_states_items_and_components_shift() {
        assert_eq!(to_26_2(&BLOCK_STATES, 28), Some(27));
        assert_eq!(to_26_3(&BLOCK_STATES, 27), Some(28));
        assert_eq!(to_26_2(&ITEMS, 73), Some(72));
        assert_eq!(to_26_3(&ITEMS, 72), Some(73));
        assert_eq!(to_26_3(&COMPONENTS, 40), None);
        assert_eq!(to_26_3(&COMPONENTS, 41), Some(42));
        assert_eq!(to_26_2(&COMPONENTS, 42), Some(41));
        assert_eq!(to_26_2(&COMPONENTS, 41), None);
        assert_eq!(to_26_2(&PARTICLES, 46), Some(43));
        assert_eq!(to_26_3(&PARTICLES, 43), Some(46));
        assert_eq!(to_26_2(&SOUNDS, 496), Some(492));
        assert_eq!(to_26_3(&SOUNDS, 492), Some(496));
    }
}
