//! Resident client chunk storage. Sections are reference-counted so worker
//! threads can mesh and light immutable snapshots while the game thread edits
//! (copy-on-write through `Arc::make_mut`).
use horizon_core::paletted::{PalettedContainer, Strategy};
use horizon_world::nbt::{Compound, Tag};
use horizon_world::registry::BlockStateId;
use std::sync::{Arc, OnceLock};

pub mod block_entity;

/// The vanilla Overworld's column, which every dimension's chunks hold for
/// now (`LevelShape::column`).
pub const MIN_Y: i32 = -64;
pub const HEIGHT: i32 = 384;
pub const SECTIONS: usize = (HEIGHT / 16) as usize;

/// A level's build range and skylight, from its dimension type (`min_y`,
/// `height`, `logical_height`, `has_skylight`), as `LevelHeightAccessor` and
/// `SerializableChunkData.write` read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelShape {
    pub sky_light: bool,
    pub min_y: i32,
    pub height: i32,
    /// `DimensionType.logicalHeight`: how high portals and chorus fruit reach.
    pub logical_height: i32,
}
impl LevelShape {
    pub const OVERWORLD: Self = Self {
        sky_light: true,
        min_y: MIN_Y,
        height: HEIGHT,
        logical_height: HEIGHT,
    };
    pub const NETHER: Self = Self {
        sky_light: false,
        min_y: 0,
        height: 256,
        logical_height: 128,
    };
    pub const END: Self = Self {
        sky_light: true,
        min_y: 0,
        height: 256,
        logical_height: 256,
    };
    /// A dimension type's shape, checked as its codec checks it.
    pub fn new(
        min_y: i32,
        height: i32,
        logical_height: i32,
        sky_light: bool,
    ) -> Result<Self, String> {
        ColumnLayout::new(min_y, height)?;
        if !(0..=height).contains(&logical_height) {
            return Err(format!(
                "logical_height {logical_height} is outside 0..={height}"
            ));
        }
        Ok(Self {
            sky_light,
            min_y,
            height,
            logical_height,
        })
    }
    /// One above the highest block (`getMaxY() + 1`).
    pub fn max_y(self) -> i32 {
        self.min_y + self.height
    }
    pub fn min_section(self) -> i32 {
        self.min_y >> 4
    }
    pub fn section_count(self) -> usize {
        (self.height >> 4) as usize
    }
    /// Whether section `y` (a section coordinate) is inside the level.
    pub fn has_section(self, y: i32) -> bool {
        (self.min_section()..self.min_section() + self.section_count() as i32).contains(&y)
    }
    /// The sections a chunk of this level holds: the level's, widened to the
    /// vanilla Overworld's `-64..320` so that vanilla's Nether and End keep
    /// the 24 sections they have always had.
    pub fn column(self) -> ColumnLayout {
        ColumnLayout::of_level(self.min_y, self.height)
    }
}

/// The shapes of a world's three levels, from its dimension types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelShapes {
    pub overworld: LevelShape,
    pub nether: LevelShape,
    pub end: LevelShape,
}
impl LevelShapes {
    pub const VANILLA: Self = Self {
        overworld: LevelShape::OVERWORLD,
        nether: LevelShape::NETHER,
        end: LevelShape::END,
    };
    /// The shape of the level `id` (`minecraft:overworld`,
    /// `minecraft:the_nether`, `minecraft:the_end`); others get the
    /// Overworld's.
    pub fn get(&self, id: &str) -> LevelShape {
        match id {
            "minecraft:the_nether" => self.nether,
            "minecraft:the_end" => self.end,
            _ => self.overworld,
        }
    }
    pub fn get_mut(&mut self, id: &str) -> &mut LevelShape {
        match id {
            "minecraft:the_nether" => &mut self.nether,
            "minecraft:the_end" => &mut self.end,
            _ => &mut self.overworld,
        }
    }
}
impl Default for LevelShapes {
    fn default() -> Self {
        Self::VANILLA
    }
}

/// The span of block Y a chunk holds: from `min_y`, `sections` sections up
/// (`LevelShape::column`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnLayout {
    min_y: i32,
    sections: usize,
}
impl ColumnLayout {
    pub const VANILLA: Self = Self {
        min_y: MIN_Y,
        sections: SECTIONS,
    };
    /// The column of a level from `min_y`, `height` high
    /// (`LevelShape::column`).
    pub fn of_level(min_y: i32, height: i32) -> Self {
        let low = min_y.min(MIN_Y);
        let high = (min_y + height).max(MIN_Y + HEIGHT);
        Self {
            min_y: low,
            sections: ((high - low) / 16) as usize,
        }
    }
    /// `DimensionType.MIN_Y`, `MAX_Y`: 12 bits of `BlockPos` Y, less 32.
    pub const LOWEST_Y: i32 = -2032;
    pub const HIGHEST_Y: i32 = 2031;
    /// A dimension type's `min_y` and `height`, as its codec accepts them:
    /// multiples of 16, at least 16 high, within `LOWEST_Y..=HIGHEST_Y`.
    pub fn new(min_y: i32, height: i32) -> Result<Self, String> {
        if min_y % 16 != 0 || height % 16 != 0 {
            return Err(format!(
                "min_y {min_y} and height {height} must be multiples of 16"
            ));
        }
        if height < 16 {
            return Err(format!("height {height} is below 16"));
        }
        if min_y < Self::LOWEST_Y
            || i64::from(min_y) + i64::from(height) > i64::from(Self::HIGHEST_Y) + 1
        {
            return Err(format!(
                "min_y {min_y} with height {height} leaves {}..={}",
                Self::LOWEST_Y,
                Self::HIGHEST_Y
            ));
        }
        Ok(Self {
            min_y,
            sections: (height / 16) as usize,
        })
    }
    pub fn min_y(self) -> i32 {
        self.min_y
    }
    /// One above the highest block.
    pub fn max_y(self) -> i32 {
        self.min_y + self.height()
    }
    pub fn height(self) -> i32 {
        self.sections as i32 * 16
    }
    pub fn sections(self) -> usize {
        self.sections
    }
    /// The index of the section holding block `y`, if the column holds it.
    pub fn section_index(self, y: i32) -> Option<usize> {
        usize::try_from((y - self.min_y) >> 4)
            .ok()
            .filter(|&s| s < self.sections)
    }
    /// The minimum block Y of section `index`.
    pub fn section_y(self, index: usize) -> i32 {
        self.min_y + index as i32 * 16
    }
}
/// `true` for each section of `layout`: what callers that keep no
/// per-section record (of fluids, say) pass as "every section may have it".
pub fn every_section(layout: ColumnLayout) -> &'static [bool] {
    static ALL: [bool; crate::section_mask::SectionMask::CAPACITY] =
        [true; crate::section_mask::SectionMask::CAPACITY];
    &ALL[..layout.sections()]
}
impl Default for ColumnLayout {
    fn default() -> Self {
        Self::VANILLA
    }
}

/// Biomes are 4x4x4 containers over the biome table (67 entries: a 7-bit
/// global palette).
pub const BIOME_STRATEGY: Strategy = Strategy::biomes(8);

/// Bits of the block states' global palette: 16 for vanilla's 35,723 states
/// (what vanilla servers and clients send), more when content mods add states
/// past 65,536 (docs/modpacks.md). Set once, before the first chunk.
static BLOCK_BITS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(16);

/// Block states are `PalettedContainer`s over the registry: vanilla's 16-bit
/// global palette, wider with enough content mods.
pub fn block_strategy() -> Strategy {
    Strategy::blocks(BLOCK_BITS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Frees `columns` (`count` of them) on a short-lived thread of its own
/// when there are many: a teleport or a dimension change drops a few
/// thousand columns at once, and freeing their sections took the frame
/// 15-20 ms. A few are freed here.
pub fn free_columns<T: Send + 'static>(columns: T, count: usize) {
    if count < 64 {
        return;
    }
    // Should the thread not start, the closure drops `columns` here.
    let _ = horizon_os::thread::Builder::new()
        .name("Chunk freeing".into())
        .spawn(move || drop(columns));
}

/// Sizes the block states' global palette for a registry of `states`.
pub fn set_block_states(states: usize) {
    let bits = horizon_core::paletted::ceil_log2(states).max(16);
    BLOCK_BITS.store(bits, std::sync::atomic::Ordering::Relaxed);
    forget_shared();
}

/// One allocation per kind of uniform section (a single block, a single
/// biome and one light value), shared by every column that has it, keyed by
/// (block, biome, light). A tall column is mostly such sections: air with
/// full sky light above the terrain. Edits copy them (`Arc::make_mut`).
type SharedSections = std::collections::HashMap<(u32, u32, u8), Arc<Section>>;
static SHARED: std::sync::Mutex<Option<SharedSections>> = std::sync::Mutex::new(None);

/// Replaces the uniform sections of `sections` with the shared ones.
pub fn share_uniform(sections: &mut [Arc<Section>]) {
    let key = |s: &Section| -> Option<(u32, u32, u8)> {
        let Light::Uniform(light) = s.light else {
            return None;
        };
        if s.blocks.bits() != 0 || s.biomes.bits() != 0 {
            return None;
        }
        Some((s.blocks.uniform()?, s.biomes.uniform()?, light))
    };
    let Ok(mut shared) = SHARED.lock() else {
        return;
    };
    let shared = shared.get_or_insert_with(Default::default);
    for section in sections {
        let Some(k) = key(section) else {
            continue;
        };
        let one = shared.entry(k).or_insert_with(|| section.clone());
        if !Arc::ptr_eq(one, section) {
            *section = one.clone();
        }
    }
}

/// Forgets the shared sections, when the palette changes.
fn forget_shared() {
    if let Ok(mut shared) = SHARED.lock() {
        *shared = None;
    }
}
/// Cells in one section's light (16x16x16).
pub const LIGHT_CELLS: usize = 4096;
/// One nibble half of a section's light: two cells a byte, the even one in the
/// low nibble, as vanilla's `DataLayer` holds them.
pub const LIGHT_HALF: usize = LIGHT_CELLS / 2;

/// A section's light that is not one value everywhere: sky light and block
/// light in halves of their own, each left out while it is all zero. With
/// both present they hold exactly the packed cells.
#[derive(Clone, Debug, PartialEq)]
pub struct FullLight {
    /// The high nibbles (sky light), `None` when every one is zero.
    sky: Option<Box<[u8; LIGHT_HALF]>>,
    /// The low nibbles (block light), `None` when every one is zero.
    block: Option<Box<[u8; LIGHT_HALF]>>,
}
impl FullLight {
    /// Every cell `value`; a zero half is not stored.
    pub fn filled(value: u8) -> Self {
        let half = |nibble: u8| (nibble != 0).then(|| Box::new([nibble | nibble << 4; LIGHT_HALF]));
        Self {
            sky: half(value >> 4),
            block: half(value & 15),
        }
    }
    /// From packed cells: `packed[i]` is cell `i`'s sky light in the high
    /// nibble and block light in the low one.
    pub fn from_packed(packed: &[u8; LIGHT_CELLS]) -> Self {
        let mut sky = Box::new([0u8; LIGHT_HALF]);
        let mut block = Box::new([0u8; LIGHT_HALF]);
        let (mut any_sky, mut any_block) = (false, false);
        for (i, cell) in packed.as_chunks::<2>().0.iter().enumerate() {
            let [even, odd] = *cell;
            sky[i] = even >> 4 | odd & 0xF0;
            block[i] = even & 15 | odd << 4;
            any_sky |= sky[i] != 0;
            any_block |= block[i] != 0;
        }
        Self {
            sky: any_sky.then_some(sky),
            block: any_block.then_some(block),
        }
    }
    /// The packed cells, as the light arrays of a save or a packet hold them.
    pub fn to_packed(&self) -> Box<[u8; LIGHT_CELLS]> {
        let mut out = Box::new([0u8; LIGHT_CELLS]);
        for (i, cell) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let sky = self.sky.as_ref().map_or(0, |a| a[i]);
            let block = self.block.as_ref().map_or(0, |a| a[i]);
            cell[0] = (sky & 15) << 4 | (block & 15);
            cell[1] = sky & 0xF0 | block >> 4;
        }
        out
    }
    #[inline]
    pub fn get(&self, i: usize) -> u8 {
        let (byte, shift) = (i >> 1, (i & 1) * 4);
        let sky = self.sky.as_ref().map_or(0, |a| a[byte] >> shift & 15);
        let block = self.block.as_ref().map_or(0, |a| a[byte] >> shift & 15);
        sky << 4 | block
    }
    /// Sets one cell, keeping the others; a half left all zero gives its
    /// bytes back.
    pub fn set(&mut self, i: usize, value: u8) {
        let (byte, shift) = (i >> 1, (i & 1) * 4);
        set_nibble(&mut self.sky, byte, shift, value >> 4);
        set_nibble(&mut self.block, byte, shift, value & 15);
    }
    /// The one value every cell has, when they all agree.
    pub fn uniform(&self) -> Option<u8> {
        let half = |half: &Option<Box<[u8; LIGHT_HALF]>>| -> Option<u8> {
            let Some(a) = half else {
                return Some(0);
            };
            let first = a[0];
            (first & 15 == first >> 4 && a.iter().all(|&v| v == first)).then_some(first & 15)
        };
        Some(half(&self.sky)? << 4 | half(&self.block)?)
    }
    /// Whether the sky or the block half holds any light (a stored half of
    /// zero nibbles is dropped, so a half present is a half with light).
    pub fn any(&self, sky: bool) -> bool {
        if sky {
            self.sky.is_some()
        } else {
            self.block.is_some()
        }
    }
    /// Whether every cell of the sky or the block half has light `level`.
    pub fn all(&self, level: u8, sky: bool) -> bool {
        let half = if sky { &self.sky } else { &self.block };
        match half {
            Some(a) => a.iter().all(|&v| v == level << 4 | level),
            None => level == 0,
        }
    }
    /// The half's `DataLayer` bytes (two cells a byte, the even one in the low
    /// nibble), `None` while it is all zero.
    pub fn layer(&self, sky: bool) -> Option<&[u8; LIGHT_HALF]> {
        if sky {
            self.sky.as_deref()
        } else {
            self.block.as_deref()
        }
    }
    /// Heap bytes held: 2048 per stored half.
    pub fn heap_bytes(&self) -> usize {
        (self.sky.is_some() as usize + self.block.is_some() as usize) * LIGHT_HALF
    }
}

/// Writes one nibble of `half`, storing the half when it was all zero and
/// dropping it again when this made it all zero.
fn set_nibble(half: &mut Option<Box<[u8; LIGHT_HALF]>>, byte: usize, shift: usize, nibble: u8) {
    match half {
        Some(a) => a[byte] = a[byte] & !(15 << shift) | nibble << shift,
        None if nibble != 0 => {
            let mut a = Box::new([0u8; LIGHT_HALF]);
            a[byte] = nibble << shift;
            *half = Some(a);
        }
        None => return,
    }
    if nibble == 0
        && let Some(a) = half
        && a.iter().all(|&v| v == 0)
    {
        *half = None;
    }
}

/// Packed light per block: sky light in the high nibble, block light in the low one.
#[derive(Clone, Debug, PartialEq)]
pub enum Light {
    Uniform(u8),
    Full(FullLight),
}
impl Light {
    /// Light from packed cells (sky light `>> 4`, block light `& 15`), the
    /// form a save or a packet holds.
    pub fn from_packed(packed: &[u8; LIGHT_CELLS]) -> Self {
        Self::Full(FullLight::from_packed(packed))
    }
    /// The cells as packed bytes: exactly the values `get` gives.
    pub fn to_packed(&self) -> Box<[u8; LIGHT_CELLS]> {
        match self {
            Self::Uniform(v) => Box::new([*v; LIGHT_CELLS]),
            Self::Full(f) => f.to_packed(),
        }
    }
    #[inline]
    pub fn get(&self, i: usize) -> u8 {
        match self {
            Self::Uniform(v) => *v,
            Self::Full(f) => f.get(i),
        }
    }
    /// Sets one cell, keeping the others.
    pub fn set(&mut self, i: usize, value: u8) {
        match self {
            Self::Uniform(v) => {
                if *v == value {
                    return;
                }
                let mut full = FullLight::filled(*v);
                full.set(i, value);
                *self = Self::Full(full);
            }
            Self::Full(f) => f.set(i, value),
        }
    }
    /// Whether the sky or the block half holds any light.
    pub fn any(&self, sky: bool) -> bool {
        match self {
            Self::Uniform(v) => {
                if sky {
                    v >> 4 != 0
                } else {
                    v & 15 != 0
                }
            }
            Self::Full(f) => f.any(sky),
        }
    }
    /// Whether every cell of the sky or the block half has light `level`.
    pub fn all(&self, level: u8, sky: bool) -> bool {
        match self {
            Self::Uniform(v) => (if sky { v >> 4 } else { v & 15 }) == level,
            Self::Full(f) => f.all(level, sky),
        }
    }
    /// The half's `DataLayer` bytes, `None` for a uniform section or an all
    /// zero half (both all zero).
    pub fn layer(&self, sky: bool) -> Option<&[u8; LIGHT_HALF]> {
        match self {
            Self::Uniform(_) => None,
            Self::Full(f) => f.layer(sky),
        }
    }
    /// Heap bytes held: 0 when uniform, 2048 per stored half.
    pub fn heap_bytes(&self) -> usize {
        match self {
            Self::Uniform(_) => 0,
            Self::Full(f) => f.heap_bytes(),
        }
    }
}
#[inline]
fn column(x: i32, z: i32) -> usize {
    ((z & 15) << 4 | (x & 15)) as usize
}
#[inline]
pub fn index(x: i32, y: i32, z: i32) -> usize {
    ((y & 15) << 8 | (z & 15) << 4 | (x & 15)) as usize
}
#[derive(Clone)]
pub struct Section {
    pub blocks: PalettedContainer,
    /// Biome table index per 4x4x4 cell, `(y << 2 | z) << 2 | x`.
    pub biomes: PalettedContainer,
    pub light: Light,
}
impl Default for Section {
    fn default() -> Self {
        Self {
            blocks: PalettedContainer::new(block_strategy(), 0),
            biomes: PalettedContainer::new(BIOME_STRATEGY, 0),
            light: Light::Uniform(0xF0),
        }
    }
}
impl Section {
    #[inline]
    pub fn get(&self, i: usize) -> u32 {
        self.blocks.get(i)
    }
    pub fn set(&mut self, i: usize, v: u32) -> u32 {
        self.blocks.set(i, v)
    }
    /// All 4,096 block states in index order.
    pub fn unpack(&self, out: &mut [u32; 4096]) {
        self.blocks.unpack(out);
    }
    /// The block states of a whole section at once.
    pub fn from_blocks(values: &[u32; 4096]) -> PalettedContainer {
        PalettedContainer::from_values(block_strategy(), values)
    }
    #[inline]
    pub fn light(&self, i: usize) -> u8 {
        self.light.get(i)
    }
    pub fn is_uniform(&self, v: u32) -> bool {
        self.blocks.is_uniform(v)
    }
    /// Heap bytes held by the block, biome and light storage (benchmark accounting).
    pub fn heap_bytes(&self) -> usize {
        self.blocks.heap_bytes() + self.biomes.heap_bytes() + self.light.heap_bytes()
    }
    /// Collapse storage that became uniform, and drop palette entries no
    /// block uses any more.
    pub fn compact(&mut self) {
        for container in [&mut self.blocks, &mut self.biomes] {
            if container.bits() != 0
                && let Some(v) = container.uniform()
            {
                *container = PalettedContainer::new(container.strategy(), v);
            }
        }
        if let Light::Full(f) = &self.light
            && let Some(v) = f.uniform()
        {
            self.light = Light::Uniform(v);
        }
    }
}
#[derive(Clone)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    /// The minimum block Y of `sections[0]` (the layout's when it was made).
    min_y: i32,
    pub sections: Vec<Arc<Section>>,
    /// `Heightmap.Types.WORLD_SURFACE` per column (`z << 4 | x`): one above
    /// the highest non-air block, or the column's minimum Y. Built on the first read; a
    /// `set` that can move a column's surface drops it.
    surface: OnceLock<Box<[i16; 256]>>,
    /// `Heightmap.Types.MOTION_BLOCKING` per column, kept like `surface`.
    motion_blocking: OnceLock<Box<[i16; 256]>>,
    pub lit: bool,
    /// Light read from a save with `isLightOn`: no light job is needed.
    pub light_loaded: bool,
    /// Newly generated (not loaded from a save): eligible for generation-time spawns.
    pub fresh: bool,
    /// Finished from a proto chunk just now, in any kind of world, not read
    /// from a save (`ChunkEvent.Load`'s `isNewChunk`).
    pub generated: bool,
    /// Block entities read from a save, moved into the world on arrival.
    pub block_entities: Vec<((i32, i32, i32), block_entity::BlockEntity)>,
    /// `CustomName` of the container block entities (shown as the screen title).
    pub block_names: Vec<((i32, i32, i32), String)>,
    /// Generation postprocessing marks (ProtoChunk.postProcessing) as world
    /// positions, applied once by LevelChunk.postProcessGeneration.
    pub postprocess: Vec<[i32; 3]>,
    /// Saved data Horizon keeps but does not simulate (save.rs).
    pub saved: Option<Arc<ChunkExtras>>,
    /// Entities read from a save (vanilla entity NBT), spawned on arrival.
    pub entities: Vec<horizon_world::nbt::Tag>,
    /// Structure starts reaching into this chunk (`structures.References`):
    /// structure id and the start's chunk.
    pub structure_refs: Vec<(Arc<str>, (i32, i32))>,
    /// The starts generation made in this chunk (`structures.starts`), saved
    /// with the state their placement has reached by then.
    pub structure_starts: Vec<Arc<horizon_world::worldgen::structure::start::StructureStart>>,
}
impl Chunk {
    /// An all-air chunk in the current `layout`.
    pub fn empty(x: i32, z: i32) -> Self {
        Self::with_layout(x, z, ColumnLayout::VANILLA)
    }
    pub fn with_layout(x: i32, z: i32, layout: ColumnLayout) -> Self {
        let mut one = [Arc::new(Section::default())];
        share_uniform(&mut one);
        let [empty] = one;
        Self {
            x,
            z,
            min_y: layout.min_y(),
            sections: (0..layout.sections()).map(|_| empty.clone()).collect(),
            surface: OnceLock::new(),
            motion_blocking: OnceLock::new(),
            lit: false,
            light_loaded: false,
            fresh: false,
            generated: false,
            block_entities: Vec::new(),
            block_names: Vec::new(),
            structure_refs: Vec::new(),
            structure_starts: Vec::new(),
            postprocess: Vec::new(),
            saved: None,
            entities: Vec::new(),
        }
    }
    /// The minimum block Y of the column.
    #[inline]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }
    /// The column the chunk holds.
    pub fn layout(&self) -> ColumnLayout {
        ColumnLayout {
            min_y: self.min_y,
            sections: self.sections.len(),
        }
    }
    /// One above the column's highest block.
    #[inline]
    pub fn max_y(&self) -> i32 {
        self.min_y + self.sections.len() as i32 * 16
    }
    /// The index of the section holding block `y`, if the column holds it.
    #[inline]
    pub fn section_index(&self, y: i32) -> Option<usize> {
        usize::try_from((y - self.min_y) >> 4)
            .ok()
            .filter(|&s| s < self.sections.len())
    }
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> u32 {
        match self.section_index(y) {
            Some(s) => self.sections[s].get(index(x, y, z)),
            None => 0,
        }
    }
    #[inline]
    pub fn block(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        BlockStateId(self.get(x, y, z))
    }
    /// The highest y where `matches`, scanning down. Sections holding only
    /// `skip` are passed over whole: `matches` must be false for it.
    pub fn highest(&self, skip: u32, mut matches: impl FnMut(i32) -> bool) -> Option<i32> {
        self.sections
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, section)| !section.is_uniform(skip))
            .find_map(|(s, _)| {
                let base = self.min_y + s as i32 * 16;
                (base..base + 16).rev().find(|&y| matches(y))
            })
    }
    pub fn set(&mut self, x: i32, y: i32, z: i32, v: u32) -> u32 {
        let Some(s) = self.section_index(y) else {
            return v;
        };
        let previous = Arc::make_mut(&mut self.sections[s]).set(index(x, y, z), v);
        // `Heightmap.update`: a write two or more below a map's height can't
        // move it. Any other change rebuilds that map on the next read.
        if previous != v {
            for map in [&mut self.surface, &mut self.motion_blocking] {
                if map
                    .get()
                    .is_some_and(|heights| y >= i32::from(heights[column(x, z)]) - 1)
                {
                    *map = OnceLock::new();
                }
            }
        }
        previous
    }
    /// `getHeight(Heightmap.Types.WORLD_SURFACE, x, z)` (`x`, `z` within the
    /// chunk): one above the highest block that isn't `air`, or the column's
    /// minimum Y.
    /// Sections must change only through `set` once this has been read.
    pub fn world_surface(&self, x: i32, z: i32, air: impl Fn(u32) -> bool) -> i32 {
        let surface = self.surface.get_or_init(|| self.prime(|v| !air(v)));
        i32::from(surface[column(x, z)])
    }
    /// `getHeight(Heightmap.Types.MOTION_BLOCKING, x, z)` (`x`, `z` within
    /// the chunk): one above the highest state where `blocks` (a motion
    /// blocking block or a fluid), or the column's minimum Y. Kept as
    /// `world_surface` is.
    pub fn motion_blocking(&self, x: i32, z: i32, blocks: impl Fn(u32) -> bool) -> i32 {
        let heights = self.motion_blocking.get_or_init(|| self.prime(blocks));
        i32::from(heights[column(x, z)])
    }
    /// `Heightmap.primeHeightmaps`: each section from the top, skipping
    /// those of one state that doesn't count, until every column is found.
    fn prime(&self, counts: impl Fn(u32) -> bool) -> Box<[i16; 256]> {
        let mut surface = Box::new([self.min_y as i16; 256]);
        let mut found = [false; 256];
        let mut left = 256;
        let mut values = [0u32; 4096];
        for (s, section) in self.sections.iter().enumerate().rev() {
            if left == 0 {
                break;
            }
            if section.blocks.uniform().is_some_and(|v| !counts(v)) {
                continue;
            }
            section.unpack(&mut values);
            let base = self.min_y + s as i32 * 16;
            for i in 0..256 {
                if found[i] {
                    continue;
                }
                if let Some(dy) = (0..16).rev().find(|&dy| counts(values[dy << 8 | i])) {
                    surface[i] = (base + dy as i32 + 1) as i16;
                    found[i] = true;
                    left -= 1;
                }
            }
        }
        surface
    }
    #[inline]
    pub fn light(&self, x: i32, y: i32, z: i32) -> u8 {
        let s = (y - self.min_y) >> 4;
        if s < 0 {
            return 0;
        }
        if s >= self.sections.len() as i32 {
            return 0xF0;
        }
        self.sections[s as usize].light(index(x, y, z))
    }
    /// Heap bytes held by the sections (benchmark accounting).
    pub fn heap_bytes(&self) -> usize {
        self.sections.iter().map(|s| s.heap_bytes()).sum::<usize>()
    }
    /// Biome table index at a block position (`ChunkAccess.getNoiseBiome` with
    /// the quart y clamped to the column).
    #[inline]
    pub fn biome(&self, x: i32, y: i32, z: i32) -> u16 {
        let qy = ((y - self.min_y) >> 2).clamp(0, self.sections.len() as i32 * 4 - 1);
        self.sections[(qy >> 2) as usize]
            .biomes
            .get(((qy & 3) << 4 | ((z & 15) >> 2) << 2 | (x & 15) >> 2) as usize) as u16
    }
    /// Sets the biome of one quart cell (`qy` counts from the column's bottom).
    pub fn set_biome_cell(&mut self, qx: i32, qy: i32, qz: i32, biome: u16) {
        let section = Arc::make_mut(&mut self.sections[(qy >> 2) as usize]);
        section.biomes.set(
            ((qy & 3) << 4 | (qz & 3) << 2 | (qx & 3)) as usize,
            u32::from(biome),
        );
    }
}

/// A proto chunk saved before it became final. Decoration writes into a
/// chunk's neighbours, and decorated chunks wait for theirs, so the chunks
/// past the finished ones hold writes that generating them again would lose
/// (whole structure pieces, trees, ores). Vanilla saves its proto chunks too.
pub struct SavedProto {
    /// Blocks, scheduled ticks (in `saved`) and post-processing marks.
    pub column: Chunk,
    /// Decorated itself (`features`), not only written to by its neighbours
    /// (`terrain`).
    pub decorated: bool,
    /// Block entity data in placement order, with `x`, `y` and `z`.
    pub block_entities: Vec<Tag>,
    /// Entities placed by structures, with absolute `Pos`.
    pub entities: Vec<Tag>,
}

/// A container's loot table and what Horizon rolled from it. Vanilla keeps
/// `LootTable` and `LootTableSeed` until the container is first opened;
/// Horizon rolls when it generates or loads the chunk, and saves the table
/// again while the container still holds exactly the rolled items.
#[derive(Clone, Debug, PartialEq)]
pub struct RolledLoot {
    pub pos: (i32, i32, i32),
    pub table: String,
    /// The saved `LootTableSeed`, written back unless 0 (trySaveLootTable).
    pub seed: Option<i64>,
    /// The rolled `Items`.
    pub items: Tag,
}
impl RolledLoot {
    /// Puts the loot table back in place of `Items` while they are still the
    /// roll; false once the container holds anything else.
    pub fn restore(&self, map: &mut Compound) -> bool {
        if map.get(&"Items".into()) != Some(&self.items) {
            return false;
        }
        map.remove(&"Items".into());
        map.insert("LootTable".into(), Tag::String(self.table.as_str().into()));
        if let Some(seed) = self.seed.filter(|s| *s != 0) {
            map.insert("LootTableSeed".into(), Tag::Long(seed));
        }
        true
    }
}

/// Save data loaded with a chunk that Horizon keeps but does not simulate.
#[derive(Clone, Debug, Default)]
pub struct ChunkExtras {
    /// Chunk root keys written back as loaded (structures, PostProcessing,
    /// InhabitedTime, blending data, ...).
    pub root: Compound,
    /// Block entities Horizon does not model.
    pub block_entities: Vec<Tag>,
    /// The NBT of the modelled block entities as loaded, to restore the fields and
    /// detail Horizon does not model when it saves them (`block_entity_nbt::overlay`).
    pub block_entity_sources: Vec<((i32, i32, i32), Tag)>,
    /// Every saved block entity's position in the order of the save's list,
    /// the order `SerializableChunkData` puts them in the chunk's maps.
    pub block_entity_order: Vec<(i32, i32, i32)>,
    /// Scheduled ticks (`block_ticks`/`fluid_ticks`): delays relative to the
    /// save time. Registered with the level's tick scheduler on arrival and
    /// replaced with the scheduler's pending ticks when saving.
    pub block_ticks: Vec<horizon_core::ticks::SavedTick>,
    pub fluid_ticks: Vec<horizon_core::ticks::SavedTick>,
    /// Game time written as `LastUpdate`.
    pub last_update: i64,
    /// Containers still holding their loot table's roll.
    pub rolled_loot: Vec<RolledLoot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes from a deterministic xorshift64: the tests need no RNG crate.
    fn random_bytes(mut seed: u64, out: &mut [u8]) {
        for byte in out {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = (seed >> 33) as u8;
        }
    }

    /// Packed cells, `(i * step) % 16` per nibble so each value occurs.
    fn packed(sky_step: u32, block_step: u32) -> [u8; LIGHT_CELLS] {
        std::array::from_fn(|i| {
            let sky = (i as u32 * sky_step % 16) as u8;
            let block = (i as u32 * block_step % 16) as u8;
            sky << 4 | block
        })
    }

    #[test]
    fn packed_light_round_trips_exactly() {
        for seed in [1, 0x2545_F491_4F6C_DD1D, 0x9E37_79B9_7F4A_7C15] {
            let mut cells = [0u8; LIGHT_CELLS];
            random_bytes(seed, &mut cells);
            let light = Light::from_packed(&cells);
            assert_eq!(&light.to_packed()[..], &cells[..], "seed {seed:#x}");
            let full = FullLight::from_packed(&cells);
            assert_eq!(&full.to_packed()[..], &cells[..], "seed {seed:#x}");
            for i in 0..LIGHT_CELLS {
                assert_eq!(light.get(i), cells[i], "seed {seed:#x} cell {i}");
            }
        }
    }

    #[test]
    fn a_zero_half_takes_no_memory() {
        let sky_only = packed(7, 0);
        let light = Light::from_packed(&sky_only);
        let Light::Full(full) = &light else {
            panic!("sky light varies");
        };
        assert!(full.any(true) && !full.any(false));
        assert_eq!(light.heap_bytes(), LIGHT_HALF);
        assert_eq!(&light.to_packed()[..], &sky_only[..]);

        let block_only = packed(0, 11);
        let light = Light::from_packed(&block_only);
        assert!(light.any(false) && !light.any(true));
        assert_eq!(light.heap_bytes(), LIGHT_HALF);
        assert_eq!(&light.to_packed()[..], &block_only[..]);

        let both = packed(3, 5);
        let light = Light::from_packed(&both);
        assert!(light.any(true) && light.any(false));
        assert_eq!(light.heap_bytes(), LIGHT_CELLS);
        assert_eq!(&light.to_packed()[..], &both[..]);

        assert_eq!(Light::from_packed(&[0; LIGHT_CELLS]).heap_bytes(), 0);
        assert_eq!(Light::Uniform(0xF0).heap_bytes(), 0);

        // Setting one cell of an all-zero section stores only the half it needs.
        let mut light = Light::from_packed(&[0; LIGHT_CELLS]);
        light.set(1000, 0x70);
        assert_eq!(light.heap_bytes(), LIGHT_HALF);
        assert_eq!(light.get(1000), 0x70);
    }

    #[test]
    fn setting_a_half_to_zero_gives_it_back() {
        let mut light = Light::from_packed(&packed(7, 0));
        assert_eq!(light.heap_bytes(), LIGHT_HALF);
        for i in 0..LIGHT_CELLS {
            light.set(i, 0);
        }
        assert_eq!(light.heap_bytes(), 0);
        assert_eq!(&light.to_packed()[..], &[0u8; LIGHT_CELLS][..]);
    }

    #[test]
    fn get_and_set_match_a_packed_oracle() {
        let mut cells = [0u8; LIGHT_CELLS];
        random_bytes(0x5DEE_CE66_D1CE, &mut cells);
        let mut light = Light::from_packed(&cells);
        let mut oracle = cells;
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..4000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let i = (seed >> 32) as usize % LIGHT_CELLS;
            let value = (seed >> 8) as u8;
            light.set(i, value);
            oracle[i] = value;
            assert_eq!(light.get(i), oracle[i], "cell {i}");
        }
        for i in 0..LIGHT_CELLS {
            assert_eq!(light.get(i), oracle[i], "cell {i}");
        }
        assert_eq!(&light.to_packed()[..], &oracle[..]);
    }

    #[test]
    fn highest_skips_only_sections_that_cannot_match() {
        let (air, cave_air, stone) = (0, 7, 1);
        let mut chunk = Chunk::empty(0, 0);
        // Stone low down, cave air (uniform in its section) above it, and a
        // section of air with one stone block higher up.
        for y in MIN_Y..MIN_Y + 40 {
            chunk.set(3, y, 4, stone);
        }
        for i in 0..4096 {
            Arc::make_mut(&mut chunk.sections[5]).set(i, cave_air);
        }
        chunk.set(3, 150, 4, stone);
        let full = |c: &Chunk, matches: &dyn Fn(u32) -> bool| {
            (MIN_Y..MIN_Y + HEIGHT)
                .rev()
                .find(|&y| matches(c.get(3, y, 4)))
        };
        for matches in [
            &(|v: u32| v != air) as &dyn Fn(u32) -> bool,
            &|v: u32| v == stone,
            &|v: u32| v == cave_air,
        ] {
            let fast = chunk.highest(air, |y| matches(chunk.get(3, y, 4)));
            assert_eq!(fast, full(&chunk, matches));
        }
        assert_eq!(
            chunk.highest(air, |y| chunk.get(3, y, 4) == stone),
            Some(150)
        );
        assert_eq!(Chunk::empty(0, 0).highest(air, |_| unreachable!()), None);
    }
    /// The kept WORLD_SURFACE heights match a scan of every column after
    /// each of a few thousand writes (air, cave air, stone, whole sections).
    #[test]
    fn world_surface_follows_writes() {
        let (air, cave_air, stone) = (0, 7, 1);
        let is_air = |v: u32| v == air || v == cave_air;
        let scan = |c: &Chunk, x: i32, z: i32| {
            (MIN_Y..MIN_Y + HEIGHT)
                .rev()
                .find(|&y| !is_air(c.get(x, y, z)))
                .map_or(MIN_Y, |y| y + 1)
        };
        let mut chunk = Chunk::empty(0, 0);
        assert_eq!(chunk.world_surface(5, 5, is_air), MIN_Y);
        for y in MIN_Y..MIN_Y + 70 {
            for i in 0..256 {
                chunk.set(i & 15, y, i >> 4, stone);
            }
        }
        for i in 0..4096 {
            Arc::make_mut(&mut chunk.sections[8]).set(i, cave_air);
        }
        let mut random = horizon_core::random::LegacyRandom::new(9);
        use horizon_core::random::RandomSource;
        for step in 0..4000 {
            let x = random.next_int_bound(16).unwrap();
            let z = random.next_int_bound(16).unwrap();
            let y = MIN_Y + 40 + random.next_int_bound(120).unwrap();
            let v = [air, cave_air, stone][random.next_int_bound(3).unwrap() as usize];
            chunk.set(x, y, z, v);
            if step % 7 == 0 {
                for i in 0..256 {
                    assert_eq!(
                        chunk.world_surface(i & 15, i >> 4, is_air),
                        scan(&chunk, i & 15, i >> 4)
                    );
                }
            }
        }
        // Out-of-range writes and unchanged states keep the map.
        let before = chunk.world_surface(3, 3, is_air);
        chunk.set(3, MIN_Y + HEIGHT, 3, stone);
        chunk.set(3, before - 1, 3, chunk.get(3, before - 1, 3));
        assert!(chunk.surface.get().is_some());
        assert_eq!(chunk.world_surface(3, 3, |_| unreachable!()), before);
    }
    /// MOTION_BLOCKING ignores states that don't count (air, grass) and
    /// follows writes as the surface map does, both maps kept at once.
    #[test]
    fn motion_blocking_follows_writes() {
        let (air, grass, water, stone) = (0, 3, 5, 1);
        let blocks = |v: u32| v == water || v == stone;
        let scan = |c: &Chunk, x: i32, z: i32| {
            (MIN_Y..MIN_Y + HEIGHT)
                .rev()
                .find(|&y| blocks(c.get(x, y, z)))
                .map_or(MIN_Y, |y| y + 1)
        };
        let mut chunk = Chunk::empty(0, 0);
        assert_eq!(chunk.motion_blocking(5, 5, blocks), MIN_Y);
        for y in MIN_Y..MIN_Y + 70 {
            for i in 0..256 {
                chunk.set(i & 15, y, i >> 4, stone);
            }
        }
        for i in 0..4096 {
            Arc::make_mut(&mut chunk.sections[8]).set(i, grass);
        }
        let mut random = horizon_core::random::LegacyRandom::new(11);
        use horizon_core::random::RandomSource;
        for step in 0..4000 {
            let x = random.next_int_bound(16).unwrap();
            let z = random.next_int_bound(16).unwrap();
            let y = MIN_Y + 40 + random.next_int_bound(120).unwrap();
            let v = [air, grass, water, stone][random.next_int_bound(4).unwrap() as usize];
            chunk.set(x, y, z, v);
            if step % 7 == 0 {
                for i in 0..256 {
                    assert_eq!(
                        chunk.motion_blocking(i & 15, i >> 4, blocks),
                        scan(&chunk, i & 15, i >> 4)
                    );
                    chunk.world_surface(i & 15, i >> 4, |v| v == air);
                }
            }
        }
    }

    #[test]
    fn shapes_follow_the_dimension_type_codec() {
        assert_eq!(ColumnLayout::new(-64, 384), Ok(ColumnLayout::VANILLA));
        assert_eq!(ColumnLayout::default(), ColumnLayout::VANILLA);
        // To The Max's Overworld: the highest top vanilla allows.
        let tall = ColumnLayout::new(-64, 2096).unwrap();
        assert_eq!((tall.sections(), tall.max_y()), (131, 2032));
        assert!(ColumnLayout::new(-2032, 4064).is_ok());
        for (min_y, height) in [(-64, 2112), (-2048, 64), (-60, 384), (0, 250), (0, 0)] {
            assert!(
                ColumnLayout::new(min_y, height).is_err(),
                "{min_y} {height}"
            );
        }
        assert_eq!(tall.section_index(-65), None);
        assert_eq!(tall.section_index(-64), Some(0));
        assert_eq!(tall.section_index(2031), Some(130));
        assert_eq!(tall.section_index(2032), None);
        assert_eq!(tall.section_y(130), 2016);
        assert_eq!(
            LevelShape::new(-64, 384, 384, true),
            Ok(LevelShape::OVERWORLD)
        );
        assert_eq!(LevelShape::new(0, 256, 128, false), Ok(LevelShape::NETHER));
        assert!(LevelShape::new(0, 256, 257, true).is_err());
        assert!(LevelShape::new(-60, 384, 384, true).is_err());
    }

    #[test]
    fn vanilla_levels_keep_the_overworld_column() {
        for shape in [LevelShape::OVERWORLD, LevelShape::NETHER, LevelShape::END] {
            assert_eq!(shape.column(), ColumnLayout::VANILLA, "{shape:?}");
        }
        let tall = LevelShape::new(-64, 2096, 2096, true).unwrap();
        assert_eq!(tall.column(), ColumnLayout::new(-64, 2096).unwrap());
        let deep = LevelShape::new(-512, 256, 256, false).unwrap();
        assert_eq!(deep.column(), ColumnLayout::new(-512, 832).unwrap());
        assert!(LevelShape::NETHER.has_section(15) && !LevelShape::NETHER.has_section(16));
        assert!(!LevelShape::NETHER.has_section(-1));
    }

    #[test]
    fn a_tall_column_stores_its_whole_height() {
        let layout = ColumnLayout::new(-64, 2096).unwrap();
        let mut chunk = Chunk::with_layout(0, 0, layout);
        assert_eq!(
            (chunk.min_y(), chunk.max_y(), chunk.sections.len()),
            (-64, 2032, 131)
        );
        let stone = 1;
        assert_eq!(chunk.set(5, 2031, 6, stone), 0);
        assert_eq!(chunk.set(5, 2032, 6, stone), stone, "above the column");
        assert_eq!(chunk.get(5, 2031, 6), stone);
        assert_eq!(chunk.get(5, 2032, 6), 0);
        assert_eq!(chunk.world_surface(5, 6, |v| v == 0), 2032);
        assert_eq!(chunk.world_surface(0, 0, |v| v == 0), -64);
        assert_eq!(chunk.highest(0, |y| y < 1000), None);
        chunk.set(5, 999, 6, stone);
        assert_eq!(chunk.highest(0, |y| y < 1000), Some(999));
        assert_eq!(chunk.light(0, 2100, 0), 0xF0);
        assert_eq!(chunk.light(0, -65, 0), 0);
        chunk.set_biome_cell(0, 130 * 4 + 3, 0, 9);
        assert_eq!(chunk.biome(0, 5000, 0), 9);
    }
}
