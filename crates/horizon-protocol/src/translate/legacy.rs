//! Protocol 47 (Minecraft 1.8) encodings for a shared world. A 1.8 guest
//! predates the flattening, so blocks travel as a numeric id and a metadata
//! nibble, and a chunk section is that pair for each of its 4096 blocks. The
//! pair comes from the in-tree flattening table, read backwards
//! ([`horizon_world::datafix::legacy_block`]); blocks 1.9 added (ids 198 and
//! up) stand in as stone, as an unknown block does.
pub mod entities;
pub mod ids;
pub mod items;
pub mod packets;
pub mod play;

use crate::buf::write_varint;
use horizon_world::datafix;

/// Minecraft protocol 47 (1.8), the guest version translated so far.
pub const PROTOCOL_1_8: u16 = 47;
/// The last block id 1.8 has; 1.9's additions start at 198.
const LAST_1_8_ID: u8 = 197;

/// A modern biome as 1.8's number. Biomes 1.9 added (and the cave and peak
/// biomes) take the closest 1.8 one; an unknown name is plains (1).
pub fn biome(name: &str) -> u8 {
    match name.rsplit(':').next().unwrap_or(name) {
        "ocean" => 0,
        "plains" | "sunflower_plains" | "meadow" | "lush_caves" | "dripstone_caves"
        | "deep_dark" => 1,
        "desert" => 2,
        "windswept_hills"
        | "mountains"
        | "windswept_gravelly_hills"
        | "gravelly_mountains"
        | "windswept_forest"
        | "wooded_mountains"
        | "stony_peaks" => 3,
        "forest" | "flower_forest" | "pale_garden" | "cherry_grove" => 4,
        "taiga" | "grove" => 5,
        "swamp" | "swampland" | "mangrove_swamp" => 6,
        "river" => 7,
        "nether_wastes" | "the_nether" | "hell" | "soul_sand_valley" | "crimson_forest"
        | "warped_forest" | "basalt_deltas" => 8,
        "the_end" | "sky" | "end_barrens" | "end_highlands" | "end_midlands"
        | "small_end_islands" => 9,
        "frozen_ocean" => 10,
        "frozen_river" => 11,
        "snowy_plains" | "ice_plains" => 12,
        "snowy_mountains" | "ice_mountains" | "snowy_slopes" | "jagged_peaks" | "frozen_peaks" => {
            13
        }
        "mushroom_fields" | "mushroom_island" => 14,
        "mushroom_field_shore" | "mushroom_island_shore" => 15,
        "beach" => 16,
        "jungle" | "bamboo_jungle" => 21,
        "sparse_jungle" | "jungle_edge" => 23,
        "deep_ocean" => 24,
        "stony_shore" | "stone_beach" => 25,
        "snowy_beach" | "cold_beach" => 26,
        "birch_forest" => 27,
        "old_growth_birch_forest" | "birch_forest_hills" => 28,
        "dark_forest" | "roofed_forest" => 29,
        "snowy_taiga" | "cold_taiga" => 30,
        "old_growth_pine_taiga"
        | "old_growth_spruce_taiga"
        | "giant_tree_taiga"
        | "giant_spruce_taiga" => 32,
        "savanna" | "windswept_savanna" | "shattered_savanna" => 35,
        "savanna_plateau" => 36,
        "badlands" | "mesa" => 37,
        "wooded_badlands" | "mesa_plateau_stone" => 38,
        "eroded_badlands" => 39,
        _ => 1,
    }
}

/// The block a 1.8 guest sees where the world has one it does not know
/// (vanilla's own fallback for an unknown block is stone).
pub const FALLBACK: LegacyState = LegacyState { id: 1, data: 0 };

/// A block state as protocol 47 carries it: the numeric block id and its
/// metadata nibble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegacyState {
    pub id: u8,
    pub data: u8,
}

impl LegacyState {
    pub const AIR: Self = Self { id: 0, data: 0 };

    /// A modern block state as 1.8 carries it: the id and metadata the
    /// flattening table came from, or [`FALLBACK`] for a block 1.8 did not
    /// have (or one renamed since the table was written).
    pub fn of(name: &str, props: &[(&str, &str)]) -> Self {
        match datafix::legacy_block(name, props) {
            Some((id, data)) if id <= LAST_1_8_ID => Self { id, data },
            _ => FALLBACK,
        }
    }

    /// `(id << 4) | data`: the short a 1.8 chunk section stores.
    pub fn packed(self) -> u16 {
        u16::from(self.id) << 4 | u16::from(self.data)
    }
}

/// One 1.8 chunk section's blocks: 4096 packed shorts, big-endian.
pub fn section_blocks(states: &[LegacyState; 4096]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8192);
    for state in states {
        out.extend_from_slice(&state.packed().to_be_bytes());
    }
    out
}

/// One 1.8 chunk section: its blocks, then its block-light nibbles and, in a
/// dimension with sky light, the sky-light ones (2048 bytes each, two blocks
/// per byte, low nibble first).
pub fn section(
    states: &[LegacyState; 4096],
    block_light: &[u8; 2048],
    sky_light: Option<&[u8; 2048]>,
) -> Vec<u8> {
    let mut out = section_blocks(states);
    out.extend_from_slice(block_light);
    if let Some(sky) = sky_light {
        out.extend_from_slice(sky);
    }
    out
}

/// A section's 4096 block state ids as 1.8 states, through a mapper that
/// already turns a modern id into 1.8's (the shared-world host's resolver).
pub fn states_of(
    blocks: impl Fn(usize) -> u32,
    resolve: impl Fn(u32) -> LegacyState,
) -> [LegacyState; 4096] {
    let mut out = [LegacyState::AIR; 4096];
    for (index, state) in out.iter_mut().enumerate() {
        *state = resolve(blocks(index));
    }
    out
}

/// A 26.3 section's 4096 block state ids as 1.8 states, each resolved through
/// `resolve` (the block registry's name and properties). The caller supplies
/// the ids by index so this module need not know the section type.
pub fn states_from(
    blocks: impl Fn(usize) -> u32,
    resolve: impl Fn(u32) -> (String, Vec<(String, String)>),
) -> [LegacyState; 4096] {
    let mut out = [LegacyState::AIR; 4096];
    for (index, state) in out.iter_mut().enumerate() {
        let (name, properties) = resolve(blocks(index));
        let properties: Vec<(&str, &str)> = properties
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        *state = LegacyState::of(&name, &properties);
    }
    out
}

/// A section's packed light (`sky << 4 | block`, index `y << 8 | z << 4 | x`)
/// as 1.8's two nibble arrays.
pub fn light_nibbles(light: impl Fn(usize) -> u8) -> ([u8; 2048], [u8; 2048]) {
    let mut block = [0u8; 2048];
    let mut sky = [0u8; 2048];
    for index in 0..4096 {
        let packed = light(index);
        let byte = index / 2;
        let shift = ((index % 2) * 4) as u32;
        block[byte] |= (packed & 15) << shift;
        sky[byte] |= (packed >> 4) << shift;
    }
    (block, sky)
}

/// A whole 1.8 section from a 26.3 one: states, then tests-callable light.
pub fn section_from(
    states: &[LegacyState; 4096],
    light: impl Fn(usize) -> u8,
    sky_light: bool,
) -> Vec<u8> {
    let (block, sky) = light_nibbles(light);
    section(states, &block, sky_light.then_some(&sky))
}

/// A protocol 47 `Chunk Data` packet body (`S21PacketChunkData`): the chunk's
/// column, whether the data is ground-up, the section mask, the section data
/// of each set bit in order, and the biomes when ground-up.
pub fn chunk_data_body(
    x: i32,
    z: i32,
    ground_up: bool,
    mask: u16,
    sections: &[Vec<u8>],
    biomes: Option<&[u8; 256]>,
) -> Vec<u8> {
    let mut data = Vec::new();
    for section in sections {
        data.extend_from_slice(section);
    }
    if ground_up {
        data.extend_from_slice(biomes.unwrap_or(&[0; 256]));
    }
    let mut out = Vec::with_capacity(13 + data.len());
    out.extend_from_slice(&x.to_be_bytes());
    out.extend_from_slice(&z.to_be_bytes());
    out.push(u8::from(ground_up));
    out.extend_from_slice(&mask.to_be_bytes());
    write_varint(&mut out, data.len() as i32);
    out.extend_from_slice(&data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_pack_like_1_8() {
        assert_eq!(
            LegacyState::of("minecraft:stone", &[]),
            LegacyState { id: 1, data: 0 }
        );
        assert_eq!(LegacyState::AIR.packed(), 0);
        assert_eq!(LegacyState { id: 1, data: 3 }.packed(), 0x13);
        // Grass kept its id and snowy metadata through the flattening.
        assert_eq!(
            LegacyState::of("minecraft:grass_block", &[("snowy", "false")]),
            LegacyState { id: 2, data: 0 }
        );
    }

    #[test]
    fn biomes_use_1_8s_numbers() {
        assert_eq!(biome("minecraft:ocean"), 0);
        assert_eq!(biome("minecraft:plains"), 1);
        assert_eq!(biome("minecraft:the_nether"), 8);
        assert_eq!(biome("minecraft:the_end"), 9);
        assert_eq!(biome("minecraft:deep_ocean"), 24);
        assert_eq!(biome("minecraft:snowy_taiga"), 30);
        // A 1.9+ or unknown biome takes the closest old one.
        assert_eq!(biome("minecraft:lush_caves"), 1);
        assert_eq!(biome("example:unknown"), 1);
    }

    #[test]
    fn later_blocks_and_unknown_ones_stand_in_as_stone() {
        // End rods came with 1.9 (id 198).
        assert_eq!(LegacyState::of("minecraft:end_rod", &[]), FALLBACK);
        assert_eq!(LegacyState::of("minecraft:not_a_block", &[]), FALLBACK);
    }

    #[test]
    fn a_section_is_blocks_then_both_lights() {
        let mut states = [LegacyState { id: 1, data: 3 }; 4096];
        states[4095] = LegacyState { id: 5, data: 1 };
        let block_light = [0xAA; 2048];
        let sky_light = [0x55; 2048];
        let bytes = section(&states, &block_light, Some(&sky_light));
        assert_eq!(bytes.len(), 8192 + 2048 + 2048);
        assert_eq!(&bytes[..2], &[0x00, 0x13]);
        assert_eq!(&bytes[8190..8194], &[0x00, 0x51, 0xAA, 0xAA]);
        assert_eq!(&bytes[10240..10242], &[0x55, 0x55]);
        // No sky light in the Nether or the End.
        assert_eq!(section(&states, &block_light, None).len(), 8192 + 2048);
    }

    #[test]
    fn a_section_resolves_its_states_and_light() {
        // Stone everywhere, air at one index; packed light varying.
        let states = states_from(
            |index| if index == 7 { 0 } else { 1 },
            |id| match id {
                0 => ("minecraft:air".to_owned(), Vec::new()),
                _ => ("minecraft:stone".to_owned(), Vec::new()),
            },
        );
        assert_eq!(states[7], LegacyState::AIR);
        assert_eq!(states[6], LegacyState { id: 1, data: 0 });
        let (block, sky) = light_nibbles(|index| match index {
            0 => 0x21,
            1 => 0x43,
            _ => 0,
        });
        assert_eq!((block[0], sky[0]), (0x31, 0x42));
        assert_eq!(block[1], 0);
        let bytes = section_from(&states, |_| 0x21, false);
        assert_eq!(bytes.len(), 8192 + 2048);
        assert_eq!(&bytes[8190..8192], &[0x00, 0x10]);
        assert_eq!(&bytes[8192], &0x11);
    }

    #[test]
    fn chunk_data_frames_its_sections_and_biomes() {
        let body = chunk_data_body(-1, 2, true, 0x0003, &[vec![1, 2], vec![3]], None);
        assert_eq!(&body[..4], &(-1i32).to_be_bytes());
        assert_eq!(&body[4..8], &2i32.to_be_bytes());
        assert_eq!(body[8], 1);
        assert_eq!(&body[9..11], &[0x00, 0x03]);
        // 3 section bytes and 256 biomes: a two-byte VarInt.
        assert_eq!(&body[11..13], &[0x83, 0x02]);
        assert_eq!(&body[13..16], &[1, 2, 3]);
        assert_eq!(body.len(), 13 + 3 + 256);
    }
}
