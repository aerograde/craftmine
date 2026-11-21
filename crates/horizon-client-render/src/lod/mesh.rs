//! Exposed voxel-column faces, merged without changing their block-scale UVs.
use super::{Column, HEIGHTFIELD_CELL, Span, Spans, Surface, TileKey};
use crate::{
    fast_hash::{FxHashMap, FxHashSet},
    mesher::{OPAQUE_LAYER, SectionMesh, TILED_UV_STEPS, TerrainVertex, fluid_face},
    streaming::WorkerContext,
};
use horizon_assets::model::Direction;
use horizon_core::position::BlockPos;
use horizon_world::registry::BlockStateId;
use std::collections::BTreeMap;

const CELLS: usize = 16;

/// How far above the ground a canopy may start before the height-map mesher
/// treats it as an island rather than a tree.
const TREE_REACH: i16 = 8;

pub(super) struct EdgeColumn<'a> {
    pub start: i32,
    pub end: i32,
    pub column: &'a Column,
}

#[derive(Clone, Copy)]
enum Tint {
    None,
    Grass,
    Foliage,
    Dry,
    Water,
    Constant(u32),
}
#[derive(Clone, Copy)]
struct Style {
    layer: u16,
    tint: Tint,
    translucent: bool,
}
#[derive(Clone, Copy)]
struct Styles {
    faces: [Option<Style>; 2],
    fluid: u8,
    opaque: bool,
}
#[derive(Clone, Copy)]
struct Paint {
    layer: u16,
    light: u16,
    color: u32,
    translucent: bool,
}
#[derive(Clone, Copy)]
struct Material {
    faces: [Option<Paint>; 2],
    fluid: u8,
    opaque: bool,
}
#[derive(Default)]
pub(super) struct Mesher {
    styles: FxHashMap<(BlockStateId, u8), Styles>,
}
impl Mesher {
    pub(super) fn mesh(
        &mut self,
        key: TileKey,
        columns: &[&Column],
        centres: &[Option<Surface>],
        covered: &FxHashSet<(i32, i32)>,
        covered_bounds: Option<[i32; 4]>,
        context: &WorkerContext,
        neighbors: &[Vec<EdgeColumn<'_>>; 4],
        finer: &[TileKey],
        min_y: i32,
    ) -> SectionMesh {
        mesh_columns_with_edges(
            key,
            columns,
            centres,
            covered,
            covered_bounds,
            neighbors,
            finer,
            min_y,
            |span, direction, at| self.material(span, direction, at, context),
        )
    }
    /// `at`: the block column the face covers, for the grass colours that
    /// change across a biome (swamps).
    fn material(
        &mut self,
        span: &Span,
        direction: Direction,
        at: [i32; 2],
        context: &WorkerContext,
    ) -> Material {
        let styles = *self
            .styles
            .entry((span.state, direction as u8))
            .or_insert_with(|| {
                let assets = &context.assets;
                let name = assets
                    .registry
                    .state(span.state)
                    .map_or("", |s| s.name.as_str());
                let fluid = match name {
                    "minecraft:water" | "minecraft:bubble_column" => 1,
                    "minecraft:lava" => 2,
                    _ => 0,
                };
                if fluid != 0 {
                    let sprite = if fluid == 1 {
                        "block/water_still"
                    } else {
                        "block/lava_still"
                    };
                    let render = &context.render;
                    let layer = render
                        .sprite_layers
                        .get(sprite)
                        .or_else(|| render.sprite_layers.get(&format!("minecraft:{sprite}")))
                        .copied()
                        .unwrap_or(0) as u16;
                    return Styles {
                        faces: [
                            Some(Style {
                                layer,
                                tint: if fluid == 1 { Tint::Water } else { Tint::None },
                                translucent: fluid == 1,
                            }),
                            None,
                        ],
                        fluid,
                        opaque: fluid == 2,
                    };
                }
                let leaves = name.ends_with("_leaves");
                let tint = match name.strip_prefix("minecraft:").unwrap_or(name) {
                    "spruce_leaves" => Tint::Constant(0x61_9961),
                    "birch_leaves" => Tint::Constant(0x80_A755),
                    // Cherry (and other pre-coloured) leaves: the texture is
                    // already pink; biome foliage tint turns a grove yellow (#4688).
                    "cherry_leaves"
                    | "azalea_leaves"
                    | "flowering_azalea_leaves"
                    | "pale_oak_leaves" => Tint::None,
                    "leaf_litter" => Tint::Dry,
                    _ if leaves => Tint::Foliage,
                    _ => Tint::Grass,
                };
                let models = context
                    .render
                    .model_catalog
                    .collect(span.state, BlockPos::new(0, 0, 0))
                    .ok();
                let opaque = leaves
                    || models
                        .iter()
                        .flatten()
                        .flat_map(|(_, model)| &model.faces)
                        .any(|face| !face.translucent);
                let mut matching = models
                    .iter()
                    .flatten()
                    .flat_map(|(_, model)| &model.faces)
                    .filter(|face| face.quad.facing == direction);
                let mut faces = [None; 2];
                for slot in &mut faces {
                    let Some(face) = matching.next() else { break };
                    *slot = Some(Style {
                        layer: face.layer as u16 | if leaves { OPAQUE_LAYER } else { 0 },
                        tint: if face.quad.tint_index >= 0 {
                            tint
                        } else {
                            Tint::None
                        },
                        translucent: face.translucent && !leaves,
                    });
                    // Grass's tinted side overlay must follow its untinted base.
                    if name != "minecraft:grass_block"
                        || matches!(direction, Direction::Up | Direction::Down)
                    {
                        break;
                    }
                }
                Styles {
                    opaque,
                    faces,
                    fluid,
                }
            });
        let biome = context.biomes.get(span.biome);
        let shade = match direction {
            Direction::Up => 1.0,
            Direction::Down => 0.5,
            Direction::North | Direction::South => 0.8,
            Direction::West | Direction::East => 0.6,
        };
        Material {
            faces: styles.faces.map(|face| {
                face.map(|face| {
                    let rgb = match face.tint {
                        Tint::None => 0xffffff,
                        Tint::Grass => {
                            context
                                .biomes
                                .grass(span.biome, f64::from(at[0]), f64::from(at[1]))
                        }
                        Tint::Foliage => biome.foliage,
                        Tint::Dry => biome.dry_foliage,
                        Tint::Water => biome.water,
                        Tint::Constant(color) => color,
                    };
                    let channel = |shift: u32| (((rgb >> shift) & 255u32) as f32 * shade) as u32;
                    Paint {
                        layer: face.layer,
                        light: span.light,
                        color: channel(16) | channel(8) << 8 | channel(0) << 16 | 0xff000000,
                        translucent: face.translucent,
                    }
                })
            }),
            fluid: styles.fluid,
            opaque: styles.opaque,
        }
    }
}

struct Segment<'a> {
    span: &'a Span,
    min: f32,
    max: f32,
    material: Material,
    top: bool,
    bottom: bool,
}
fn hides(neighbor: &Span, material: Material, current: &Span, current_material: Material) -> bool {
    material.opaque
        || neighbor.state == current.state
        || (material.fluid != 0 && material.fluid == current_material.fluid)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FaceKey {
    direction: Direction,
    plane: u32,
    overlay: u8,
    layer: u16,
    light: u16,
    color: u32,
    translucent: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
}
type Faces = BTreeMap<FaceKey, Vec<Rect>>;
fn add_face(faces: &mut Faces, direction: Direction, plane: f32, rect: Rect, material: Material) {
    for (overlay, paint) in material.faces.into_iter().enumerate() {
        let Some(paint) = paint else { continue };
        // The same fluid flags the section mesher sets, so the material stage's water
        // reaches the distant tiles too and the LOD boundary keeps one look.
        let color = if material.fluid != 0 {
            fluid_face(paint.color, direction as usize)
        } else {
            paint.color
        };
        faces
            .entry(FaceKey {
                direction,
                plane: plane.to_bits(),
                overlay: overlay as u8,
                layer: paint.layer,
                light: paint.light,
                color,
                translucent: paint.translucent,
            })
            .or_default()
            .push(rect);
    }
}

#[cfg(test)]
fn mesh_columns(
    key: TileKey,
    columns: &[Column],
    covered: &FxHashSet<(i32, i32)>,
    covered_bounds: Option<[i32; 4]>,
    material: impl FnMut(&Span, Direction, [i32; 2]) -> Material,
) -> SectionMesh {
    mesh_columns_with_neighbors(key, columns, covered, covered_bounds, &[], material)
}
/// `mesh_columns` with per-cell centre heights, for the sub-cell fan.
#[cfg(test)]
fn mesh_columns_centres(
    key: TileKey,
    columns: &[Column],
    centres: &[Option<Surface>],
    material: impl FnMut(&Span, Direction, [i32; 2]) -> Material,
) -> SectionMesh {
    let refs: Vec<&Column> = columns.iter().collect();
    mesh_columns_with_edges(
        key,
        &refs,
        centres,
        &FxHashSet::default(),
        None,
        &std::array::from_fn(|_| Vec::new()),
        &[],
        crate::chunk::MIN_Y,
        material,
    )
}
#[cfg(test)]
fn mesh_columns_with_neighbors(
    key: TileKey,
    columns: &[Column],
    covered: &FxHashSet<(i32, i32)>,
    covered_bounds: Option<[i32; 4]>,
    neighbors: &[Column],
    material: impl FnMut(&Span, Direction, [i32; 2]) -> Material,
) -> SectionMesh {
    assert!(neighbors.is_empty() || neighbors.len() == 4 * CELLS);
    let stride = key.size() / CELLS as i32;
    let columns: Vec<&Column> = columns.iter().collect();
    let edges = std::array::from_fn(|side| {
        neighbors
            .get(side * CELLS..(side + 1) * CELLS)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .map(|(row, column)| EdgeColumn {
                start: row as i32 * stride,
                end: (row as i32 + 1) * stride,
                column,
            })
            .collect()
    });
    mesh_columns_with_edges(
        key,
        &columns,
        &[],
        covered,
        covered_bounds,
        &edges,
        &[],
        crate::chunk::MIN_Y,
        material,
    )
}
/// From this tile level up a cell is 2 blocks wide or more: the strata inside
/// the ground cannot be told apart, so a column is its surface. The merged
/// walls are what keep the far rings to a handful of quads per cell. Level 0
/// (1-block cells) keeps its voxel columns: it is the fallback band that meets
/// the resident chunks, and its cliffs must show the same layers the real
/// terrain does.
///
/// This used to start at level 3. Levels 1–2 were then meshed as voxel columns,
/// one face per block, and the 2-block band alone produced millions of quads
/// for a silhouette no sharper than a merged heightmap at that distance. The
/// lip keeps the surface layer over the material beneath it, so the rings still
/// show their strata; only the per-block side faces go away.
const HEIGHTMAP_LEVEL: u8 = 1;
/// How far a cell's centre must stand off its corners' plane, in blocks, before
/// the cell is split into the four-triangle fan.
///
/// A cell that is flat to within a block is drawn as one quad: subdividing it
/// would add two vertices and change nothing on screen. Heights are integers,
/// so this is the smallest deviation that is real terrain rather than rounding.
const CENTRE_SUBDIVIDE: f32 = 1.0;
/// `column` as a heightmap: its ground reaching down to the bottom of the
/// world (so the walls toward lower neighbours are one face, merged along the
/// cliff), and whatever sits above it kept as it is: the sea, and a canopy
/// over the air gap under it. Stretching the topmost span down made a tree a
/// leaf pillar from bedrock, a green slab that vanished when the real chunk
/// arrived. With `lip` (the rings whose cells are still pixels wide), the
/// ground under the surface carries the cliff: a far cell's wall is the dirt
/// or stone the real chunk shows under its grass, with the surface's own layer
/// as a lip. Deeper rings merge the cliff whole: their walls are a fraction of
/// a pixel, and one face is what keeps the far horizon cheap.
fn heightmap(
    column: &Column,
    mut fluid: impl FnMut(&Span) -> bool,
    lip: bool,
    min_y: i32,
) -> Column {
    let spans = &column.spans;
    // The terrain surface, which the seed columns record; a reduction may not,
    // and then the run of solid contiguous spans from the bottom stands in. The
    // ground is the last solid span at or below it: a trunk is contiguous with
    // the ground too, and stretching the trunk down would pillar the tree.
    let ground = match column.surface.map(|surface| surface.height as i16) {
        Some(height) => spans
            .iter()
            .rposition(|span| span.min < span.max && !fluid(span) && span.max <= height),
        None => {
            let mut floor = None;
            for (i, span) in spans.iter().enumerate() {
                if span.min >= span.max || fluid(span) {
                    break;
                }
                if i > 0 && spans[i - 1].max != span.min {
                    break;
                }
                floor = Some(i);
            }
            floor
        }
    };
    let Some(top) = ground else {
        return column.clone();
    };
    let surface = spans[top];
    // The dimension's floor, not the Overworld's: the Nether starts at y = 0,
    // so stretching there drew cliffs 64 blocks below its bedrock.
    let floor = min_y as i16;
    let stretch = |span: &Span| Span {
        min: floor,
        ..*span
    };
    let mut out = Spans::with_capacity(spans.len() - top + 1);
    match lip
        .then(|| top.checked_sub(1))
        .flatten()
        .map(|under| &spans[under])
    {
        Some(under) if under.max == surface.min => {
            out.push(stretch(under));
            out.push(surface);
        }
        _ => out.push(stretch(&surface)),
    }
    // What stands above the surface: a canopy over an air gap, which is kept
    // where it is. A canopy overhangs air in the real world, and stretching
    // it down to the ground turned every tree into a leaf pillar from
    // bedrock. Spans far above the surface are something else — an island —
    // and are dropped rather than left as a plate in the sky.
    let mut reach = out.last().map_or(surface.max, |span| span.max);
    for span in &spans[top + 1..] {
        if span.min - reach > TREE_REACH {
            break;
        }
        out.push(*span);
        reach = span.max;
    }
    Column {
        surface: column.surface,
        spans: out,
    }
}
#[derive(Clone, Copy)]
struct HeightVertex {
    height: i32,
    paint: Paint,
    /// The material under the surface, for the cliff faces between cells. A
    /// wall painted with the grass top turned every far cliff into a green
    /// slab the height of the hill; the block below it is what a real chunk
    /// shows along a cliff.
    wall: Paint,
}

fn mesh_heightfield(
    key: TileKey,
    columns: &[&Column],
    centres: &[Option<Surface>],
    covered: &FxHashSet<(i32, i32)>,
    covered_bounds: Option<[i32; 4]>,
    neighbors: &[Vec<EdgeColumn<'_>>; 4],
    finer: &[TileKey],
    material: &mut impl FnMut(&Span, Direction, [i32; 2]) -> Material,
) -> SectionMesh {
    let stride = key.size() / CELLS as i32;
    let [ox, _, oz] = key.origin();
    let mut grid: Vec<Option<&Column>> = vec![None; (CELLS + 1) * (CELLS + 1)];
    for z in 0..CELLS {
        for x in 0..CELLS {
            grid[z * (CELLS + 1) + x] = Some(columns[z * CELLS + x]);
        }
    }
    let edge_column = |side: usize, offset: i32| {
        neighbors[side]
            .iter()
            .find_map(|edge| (edge.start <= offset && offset < edge.end).then_some(edge.column))
    };
    for z in 0..CELLS {
        let offset = z as i32 * stride;
        grid[z * (CELLS + 1) + CELLS] =
            edge_column(1, offset).or_else(|| grid[z * (CELLS + 1) + CELLS - 1]);
    }
    for x in 0..CELLS {
        let offset = x as i32 * stride;
        grid[CELLS * (CELLS + 1) + x] =
            edge_column(3, offset).or_else(|| grid[(CELLS - 1) * (CELLS + 1) + x]);
    }
    grid[CELLS * (CELLS + 1) + CELLS] =
        grid[CELLS * (CELLS + 1) + CELLS - 1].or(grid[(CELLS - 1) * (CELLS + 1) + CELLS]);

    let top_span = |column: &Column| -> Option<Span> {
        let surface = column.surface?;
        column
            .spans
            .iter()
            .rev()
            .find(|span| {
                i32::from(span.min) < surface.height && i32::from(span.max) >= surface.height
            })
            .or_else(|| column.spans.last())
            .map(|span| Span {
                min: (surface.height - 1) as i16,
                max: surface.height as i16,
                state: surface.state,
                biome: surface.biome,
                light: span.light,
            })
    };
    let mut vertices: Vec<Option<HeightVertex>> = Vec::with_capacity(grid.len());
    for (index, column) in grid.iter().enumerate() {
        let Some(column) = column else {
            vertices.push(None);
            continue;
        };
        let Some(surface) = column.surface else {
            vertices.push(None);
            continue;
        };
        let Some(span) = top_span(column) else {
            vertices.push(None);
            continue;
        };
        let (wx, wz) = (
            ox + (index % (CELLS + 1)) as i32 * stride,
            oz + (index / (CELLS + 1)) as i32 * stride,
        );
        let paint = material(&span, Direction::Up, [wx, wz])
            .faces
            .into_iter()
            .flatten()
            .next();
        // The block just under the surface, seen from the side, for the walls
        // between cells. A surface-only column (a sampled stand-in) falls back
        // to its own material.
        let below = column
            .spans
            .iter()
            .rev()
            .find(|below| {
                i32::from(below.min) <= surface.height - 2
                    && i32::from(below.max) >= surface.height - 1
            })
            .or_else(|| column.spans.last());
        let wall = below.and_then(|below| {
            let span = Span {
                min: (surface.height - 2) as i16,
                max: (surface.height - 1) as i16,
                ..*below
            };
            material(&span, Direction::North, [wx, wz])
                .faces
                .into_iter()
                .flatten()
                .next()
        });
        vertices.push(paint.map(|paint| HeightVertex {
            height: surface.height,
            paint,
            wall: wall.unwrap_or(paint),
        }));
    }

    // A cell is skipped only when it is entirely under resident terrain: the
    // test used to look at the cell's origin chunk alone, so a 32-block cell
    // whose first chunk was resident stopped drawing while its other three
    // chunks were not, and one whose origin chunk was *not* resident drew over
    // the resident chunks: terrain z-fighting and stray far blocks.
    let hidden = |x: i32, z: i32, size: i32| {
        // Only cells that reach the resident rectangle can touch a resident
        // chunk: the scan below is per chunk, and a 512-block cell would ask
        // it 1,024 times. Off the rectangle the answer is always "not hidden".
        let reaches_covered = covered_bounds
            .is_none_or(|[x0, z0, x1, z1]| x < x1 && x + size > x0 && z < z1 && z + size > z0);
        let chunks = |a: i32, size: i32| a.div_euclid(16)..=(a + size - 1).div_euclid(16);
        // A cell that touches a resident chunk is dropped whole: drawing it
        // over the real terrain z-fights, and the finer tiles cover that area.
        (reaches_covered
            && chunks(x, size).any(|cx| {
                chunks(z, size)
                    .clone()
                    .any(|cz| covered.contains(&(cx, cz)))
            }))
            || finer.iter().any(|fine| {
                let [fx, _, fz] = fine.origin();
                let fs = fine.size();
                fx < x + size && fx + fs > x && fz < z + size && fz + fs > z
            })
    };

    // `TerrainVertex::tiled` packs UVs at `TILED_UV_STEPS` per block into 16
    // bits, so a tile-local UV saturates 2048 blocks into the tile. The far
    // rings are 4096 to 8192 blocks wide: past that limit every vertex would
    // take the same capped UV, and its zero derivative picked mip 0, stretching
    // one texel over the whole ring. Those tiles count UVs from their own cell
    // instead: one cell is a few pixels there, so the phase reset is invisible,
    // while the derivative (and so the mip) stays right.
    let relative = key.size() as f32 > (u16::MAX as f32) / TILED_UV_STEPS;
    let mut mesh = SectionMesh::default();
    let index = |x: usize, z: usize| z * (CELLS + 1) + x;
    let push = |mesh: &mut SectionMesh,
                vertex: HeightVertex,
                pos: [f32; 3],
                uv: [f32; 2],
                translucent: bool| {
        mesh.note_sprite_layer(vertex.paint.layer);
        let out = if translucent {
            &mut mesh.translucent
        } else {
            &mut mesh.solid
        };
        out.push(TerrainVertex::tiled(
            pos,
            uv,
            vertex.paint.layer,
            vertex.paint.color,
            vertex.paint.light,
        ));
    };
    for z in 0..CELLS {
        for x in 0..CELLS {
            let wx = ox + x as i32 * stride;
            let wz = oz + z as i32 * stride;
            if hidden(wx, wz, stride) {
                continue;
            }
            let (Some(a), Some(b), Some(c), Some(d)) = (
                vertices[index(x, z)],
                vertices[index(x + 1, z)],
                vertices[index(x + 1, z + 1)],
                vertices[index(x, z + 1)],
            ) else {
                continue;
            };
            let (x0, z0) = ((x as i32 * stride) as f32, (z as i32 * stride) as f32);
            let (x1, z1) = (x0 + stride as f32, z0 + stride as f32);
            // Non-negative on both axes: `TerrainVertex::tiled` clamps to `u16`,
            // so a negated v pinned every top face to one texel row.
            let uv = |px: f32, pz: f32| {
                if relative {
                    [px - x0, z1 - pz]
                } else {
                    [px, key.size() as f32 - pz]
                }
            };
            // A cell's top is one flat quad at one height, so its edges read as
            // steps between cells instead of a continuous slope: the shared
            // corner grid interpolated a hill smaller than a cell into a ramp,
            // which is what made the far rings look like smooth blobs. The
            // lowest corner is the step a neighbouring cell drops to, so a
            // cliff stays a cliff rather than a slab floating over it. Each
            // cell pushes its own four vertices, so flattening one cannot tear
            // a neighbour's edge; the vertex, tile and draw counts are those of
            // the one-quad path.
            let h = a.height.min(b.height).min(c.height).min(d.height);
            let y = h as f32;
            let mut points = [
                ([x0, y, z1], uv(x0, z1)),
                ([x1, y, z1], uv(x1, z1)),
                ([x1, y, z0], uv(x1, z0)),
                ([x0, y, z0], uv(x0, z0)),
            ];
            let mut values = [d, c, b, a];
            // The shared index buffer always splits a quad along its 0-2
            // diagonal, so the pair of corners on that diagonal is the edge the
            // surface is hinged on: d-b here. On a saddle, or a ridge running
            // the other way, trailing over that hinge folds both triangles up
            // and the cell reads as a blob. Hinge on the other diagonal (c-a)
            // when its corners are the closer pair in height: the ridge then
            // runs along the split, and the triangles meet at it instead of
            // straddling it.
            //
            // A cyclic rotation of the four corners keeps their winding, so the
            // indices, back-face culling and the UVs are all unchanged; it only
            // moves the diagonal from d-b to c-a.
            if (c.height - a.height).abs() < (d.height - b.height).abs() {
                points.rotate_left(1);
                values.rotate_left(1);
            }
            // A cell is four corner samples joined as one quad, so a hill
            // smaller than a cell cannot exist in the mesh. The sampler can
            // give each cell a centre too (`LodSampler::centres`), which is a
            // corner of the next-finer level — so it adds a vertex that level
            // places anyway and refinement moves nothing (`lod::lattice`).
            // Where the centre stands off the corners' plane the cell is not
            // flat, and the four triangles fanned from it carry the ridge the
            // corners alone cannot.
            //
            // The fan is two ordinary four-vertex quads, (m,a,b,c) and
            // (m,c,d,a): the shared index buffer splits each on its own
            // diagonal, so no index or pipeline change is needed.
            let centre = centres.get(z * CELLS + x).copied().flatten();
            // Still the corners' mean, not the plateau: the fan's trigger must
            // stay at its old rate, or the cell would gain vertices (and cost).
            let flat = (a.height + b.height + c.height + d.height) as f32 / 4.0;
            if let Some(centre) =
                centre.filter(|centre| (centre.height as f32 - flat).abs() >= CENTRE_SUBDIVIDE)
            {
                let (mx, mz) = (x0 + stride as f32 / 2.0, z0 + stride as f32 / 2.0);
                let span = Span {
                    min: (centre.height - 1) as i16,
                    max: centre.height as i16,
                    state: centre.state,
                    biome: centre.biome,
                    light: a.paint.light,
                };
                let paint = material(&span, Direction::Up, [mx as i32 + wx, mz as i32 + wz])
                    .faces
                    .into_iter()
                    .flatten()
                    .next();
                if let Some(paint) = paint {
                    let middle = HeightVertex {
                        height: centre.height,
                        paint,
                        wall: paint,
                    };
                    // `points`/`values` are already in the emitted order, hinge
                    // rotation included, so the fan reads them directly: two
                    // quads around the ring, each with the centre as its first
                    // vertex. That keeps the outer corners exactly where the
                    // one-quad path puts them.
                    let middle_point = ([mx, centre.height as f32, mz], uv(mx, mz));
                    for pair in [[0usize, 1, 2], [2, 3, 0]] {
                        push(&mut mesh, middle, middle_point.0, middle_point.1, false);
                        for i in pair {
                            push(&mut mesh, values[i], points[i].0, points[i].1, false);
                        }
                    }
                } else {
                    for i in 0..4 {
                        push(&mut mesh, values[i], points[i].0, points[i].1, false);
                    }
                }
            } else {
                for i in 0..4 {
                    push(&mut mesh, values[i], points[i].0, points[i].1, false);
                }
            }
            if let (Some(a), Some(b), Some(c), Some(d)) = (
                grid[index(x, z)],
                grid[index(x + 1, z)],
                grid[index(x + 1, z + 1)],
                grid[index(x, z + 1)],
            ) {
                // A cell that touches water on one corner would otherwise paint a
                // flat water plate across land at this resolution: only a cell
                // whose four corners are all under water is water.
                let mut water = None;
                let mut all_water = true;
                for column in [a, b, c, d] {
                    match column.surface.and_then(|surface| surface.water) {
                        Some((height, state)) => {
                            water = Some(match water {
                                Some((highest, shown)) if highest >= height => (highest, shown),
                                _ => (height, state),
                            });
                        }
                        None => all_water = false,
                    }
                }
                if all_water && let Some((height, state)) = water {
                    let span = Span {
                        min: (height - 1) as i16,
                        max: height as i16,
                        state,
                        biome: a.surface.map_or(0, |surface| surface.biome),
                        light: 240,
                    };
                    if let Some(paint) = material(&span, Direction::Up, [wx, wz])
                        .faces
                        .into_iter()
                        .flatten()
                        .next()
                    {
                        let vertex = HeightVertex {
                            height,
                            paint,
                            wall: paint,
                        };
                        let y = height as f32 - 1.0 / 9.0;
                        let points = [
                            ([x0, y, z1], uv(x0, z1)),
                            ([x1, y, z1], uv(x1, z1)),
                            ([x1, y, z0], uv(x1, z0)),
                            ([x0, y, z0], uv(x0, z0)),
                        ];
                        for i in 0..4 {
                            push(&mut mesh, vertex, points[i].0, points[i].1, true);
                        }
                    }
                }
            }
        }
    }
    // The ground between two samples: a heightfield's top alone drew a smooth
    // lid over every cliff, which is what read as a blob. Each cell edge whose
    // neighbours differ drops a wall from the higher top to the lower one, so a
    // far cliff keeps its silhouette (and its material) instead of a bevel.
    // A vertical face's UV runs along the wall in u (so it follows whichever
    // horizontal axis the wall spans) and up the wall in v. The top faces'
    // top-down v put the whole cliff on one texel row.
    let along = |p: f32| {
        if relative {
            let cell = stride as f32;
            p - (p / cell).floor() * cell
        } else {
            p
        }
    };
    let wall =
        |mesh: &mut SectionMesh, a: ([f32; 2], HeightVertex), b: ([f32; 2], HeightVertex)| {
            let (ya, yb) = (a.1.height as f32, b.1.height as f32);
            if (ya - yb).abs() < 1.0 {
                return;
            }
            let (high, low) = if ya > yb { (ya, yb) } else { (yb, ya) };
            // A cliff face shows what lies under the surface, not the grass
            // cap: use the higher cell's under-surface material.
            let side = HeightVertex {
                paint: if ya >= yb { a.1.wall } else { b.1.wall },
                ..a.1
            };
            // The wall spans x when its two ends share a z, and z otherwise.
            let span_x = a.0[1] == b.0[1];
            let horizontal = |point: &[f32; 2]| along(if span_x { point[0] } else { point[1] });
            let uv = |point: &[f32; 2], y: f32| [horizontal(point), high - y];
            let points = [
                ([a.0[0], high, a.0[1]], uv(&a.0, high)),
                ([b.0[0], high, b.0[1]], uv(&b.0, high)),
                ([b.0[0], low, b.0[1]], uv(&b.0, low)),
                ([a.0[0], low, a.0[1]], uv(&a.0, low)),
            ];
            for point in points {
                push(mesh, side, point.0, point.1, false);
            }
        };
    for z in 0..CELLS {
        for x in 0..CELLS {
            let at = |x: usize, z: usize| {
                let (wx, wz) = (x as i32 * stride, z as i32 * stride);
                vertices[index(x, z)].map(|vertex| ([wx as f32, wz as f32], vertex))
            };
            if let (Some(a), Some(b)) = (at(x, z), at(x + 1, z)) {
                wall(&mut mesh, a, b);
            }
            if let (Some(a), Some(b)) = (at(x, z), at(x, z + 1)) {
                wall(&mut mesh, a, b);
            }
        }
    }

    // Cracks at a tile edge are sub-block: neighbouring tiles sample the same
    // lattice, so only the interpolated interior of a quad can differ. One
    // block closes them. Dropping a whole cell of wall instead turned every
    // far tile border into a visible ring, most obviously over water.
    let skirt = 1.0;
    let skirt_quad = |mesh: &mut SectionMesh,
                      start: ([f32; 2], i32),
                      end: ([f32; 2], i32),
                      vertex: HeightVertex| {
        // A skirt is the tile edge's exposed side: the under-surface material,
        // like the walls.
        let vertex = HeightVertex {
            paint: vertex.wall,
            ..vertex
        };
        let (a, b) = (start.0, end.0);
        let (ya, yb) = (start.1 as f32, end.1 as f32);
        // A skirt is a wall too: u along the tile edge, v down from its top.
        let span_x = a[1] == b[1];
        let horizontal = |point: &[f32; 2]| along(if span_x { point[0] } else { point[1] });
        let top = [
            ([a[0], ya, a[1]], [horizontal(&a), 0.0]),
            ([b[0], yb, b[1]], [horizontal(&b), 0.0]),
        ];
        let bottom = [
            ([b[0], yb - skirt, b[1]], [horizontal(&b), skirt]),
            ([a[0], ya - skirt, a[1]], [horizontal(&a), skirt]),
        ];
        for point in [top[0], top[1], bottom[0], bottom[1]] {
            push(mesh, vertex, point.0, point.1, false);
        }
        for point in [bottom[1], bottom[0], top[1], top[0]] {
            push(mesh, vertex, point.0, point.1, false);
        }
    };
    for z in 0..CELLS {
        for (x, west) in [(0usize, true), (CELLS, false)] {
            let (Some(a), Some(b)) = (vertices[index(x, z)], vertices[index(x, z + 1)]) else {
                continue;
            };
            let px = if west { 0.0 } else { key.size() as f32 };
            skirt_quad(
                &mut mesh,
                ([px, (z as i32 * stride) as f32], a.height),
                ([px, ((z as i32 + 1) * stride) as f32], b.height),
                a,
            );
        }
    }
    for x in 0..CELLS {
        for (z, north) in [(0usize, true), (CELLS, false)] {
            let (Some(a), Some(b)) = (vertices[index(x, z)], vertices[index(x + 1, z)]) else {
                continue;
            };
            let pz = if north { 0.0 } else { key.size() as f32 };
            skirt_quad(
                &mut mesh,
                ([(x as i32 * stride) as f32, pz], a.height),
                ([((x as i32 + 1) * stride) as f32, pz], b.height),
                a,
            );
        }
    }

    for v in mesh.solid.iter_mut().chain(mesh.translucent.iter_mut()) {
        v.layer |= 0x4000;
    }
    mesh.finish_sprite_layers();
    mesh
}

fn mesh_columns_with_edges(
    key: TileKey,
    columns: &[&Column],
    centres: &[Option<Surface>],
    covered: &FxHashSet<(i32, i32)>,
    covered_bounds: Option<[i32; 4]>,
    neighbors: &[Vec<EdgeColumn<'_>>; 4],
    finer: &[TileKey],
    min_y: i32,
    mut material: impl FnMut(&Span, Direction, [i32; 2]) -> Material,
) -> SectionMesh {
    assert_eq!(columns.len(), CELLS * CELLS);
    if key.size() / CELLS as i32 >= HEIGHTFIELD_CELL {
        return mesh_heightfield(
            key,
            columns,
            centres,
            covered,
            covered_bounds,
            neighbors,
            finer,
            &mut material,
        );
    }
    // The heightmap columns are built here and borrowed like the tile's own.
    let heightmap_columns: Vec<Column>;
    let heightmap_refs: Vec<&Column>;
    let heightmap_edges: [Vec<Column>; 4];
    let heightmap_neighbors: [Vec<EdgeColumn<'_>>; 4];
    let (columns, neighbors) = if key.level >= HEIGHTMAP_LEVEL {
        let [ox, _, oz] = key.origin();
        let mut fluid = |span: &Span| material(span, Direction::Up, [ox, oz]).fluid != 0;
        // The surface lip only where a cell's blocks are still pixels wide; the
        // deeper rings merge their cliffs whole.
        let lip = key.level <= 4;
        heightmap_columns = columns
            .iter()
            .map(|column| heightmap(column, &mut fluid, lip, min_y))
            .collect();
        heightmap_edges = neighbors.each_ref().map(|side| {
            side.iter()
                .map(|edge| heightmap(edge.column, &mut fluid, lip, min_y))
                .collect()
        });
        heightmap_refs = heightmap_columns.iter().collect();
        heightmap_neighbors = std::array::from_fn(|side| {
            neighbors[side]
                .iter()
                .zip(&heightmap_edges[side])
                .map(|(edge, column)| EdgeColumn {
                    start: edge.start,
                    end: edge.end,
                    column,
                })
                .collect()
        });
        (&heightmap_refs[..], &heightmap_neighbors)
    } else {
        (columns, neighbors)
    };
    let mut count = CELLS * CELLS;
    let edge_offsets = std::array::from_fn::<_, 4, _>(|side| {
        let start = count;
        count += neighbors[side].len();
        start
    });
    let stride = key.size() / CELLS as i32;
    let [ox, _, oz] = key.origin();
    // A cell meets full-detail or finer terrain: it is meshed in smaller steps.
    let cell_step = |x: usize, z: usize| {
        let (wx, wz) = (ox + x as i32 * stride, oz + z as i32 * stride);
        let overlaps = covered_bounds.is_some_and(|[x0, z0, x1, z1]| {
            wx < x1 && wx + stride > x0 && wz < z1 && wz + stride > z0
        });
        let mut step = if overlaps { stride.min(16) } else { stride };
        for other in finer {
            let [x, _, z] = other.origin();
            if wx < x + other.size() && x < wx + stride && wz < z + other.size() && z < wz + stride
            {
                step = step.min(other.size());
            }
        }
        // The cells of the coarsest tiles are thousands of blocks wide: a
        // few steps each keep the work bounded, the cut around finer terrain
        // coarse for as long as such a tile stands in for its children.
        step.max(stride / MAX_STEPS)
    };
    let filled = fill_buried_gaps(
        columns,
        neighbors,
        stride,
        |x, z| cell_step(x, z) == stride,
        |span| material(span, Direction::Up, [ox, oz]).fluid != 0,
    );
    // A cell's faces take the colours at its centre. The tile's neighbours'
    // columns only hide faces, so their colours never show.
    let centre = |i: usize| {
        if i < CELLS * CELLS {
            [
                ox + (i % CELLS) as i32 * stride + stride / 2,
                oz + (i / CELLS) as i32 * stride + stride / 2,
            ]
        } else {
            [ox, oz]
        }
    };
    let mut materials = Vec::new();
    let prepared: Vec<Vec<Segment<'_>>> = filled
        .iter()
        .map(|column| column.as_ref())
        .chain(neighbors.iter().flatten().map(|edge| edge.column))
        .enumerate()
        .map(|(index, column)| {
            let at = centre(index);
            materials.clear();
            materials.extend(column.spans.iter().map(|s| material(s, Direction::Up, at)));
            column
                .spans
                .iter()
                .enumerate()
                .filter(|(_, span)| span.min < span.max)
                .map(|(i, span)| {
                    let current = materials[i];
                    let top = !column.spans.get(i + 1).is_some_and(|next| {
                        next.min == span.max && hides(next, materials[i + 1], span, current)
                    });
                    let bottom = i == 0
                        || !(column.spans[i - 1].max == span.min
                            && hides(&column.spans[i - 1], materials[i - 1], span, current));
                    Segment {
                        span,
                        min: f32::from(span.min),
                        max: f32::from(span.max)
                            - if current.fluid != 0 && top {
                                1.0 / 9.0
                            } else {
                                0.0
                            },
                        material: current,
                        top,
                        bottom,
                    }
                })
                .collect()
        })
        .collect();
    let mut faces = Faces::new();
    let finer_keys: FxHashSet<_> = finer.iter().copied().collect();
    let mut finer_levels: Vec<_> = finer.iter().map(|key| key.level).collect();
    finer_levels.sort_unstable();
    finer_levels.dedup();
    let is_covered = |x: i32, z: i32| {
        covered.contains(&(x.div_euclid(16), z.div_euclid(16)))
            || finer_levels.iter().any(|&level| {
                let size = 16 << level;
                finer_keys.contains(&TileKey {
                    x: x.div_euclid(size),
                    z: z.div_euclid(size),
                    level,
                })
            })
    };
    for z in 0..CELLS {
        for x in 0..CELLS {
            let (wx, wz) = (ox + x as i32 * stride, oz + z as i32 * stride);
            let step = cell_step(x, z);
            let column = &prepared[z * CELLS + x];
            let own_sky = column.last().map_or(0, |top| top.span.light & 0xff);
            for dz in (0..stride).step_by(step as usize) {
                for dx in (0..stride).step_by(step as usize) {
                    if is_covered(wx + dx, wz + dz) {
                        continue;
                    }
                    let (x0, z0) = (
                        (x as i32 * stride + dx) as f32,
                        (z as i32 * stride + dz) as f32,
                    );
                    let (x1, z1) = (x0 + step as f32, z0 + step as f32);
                    for segment in column {
                        if segment.top {
                            add_face(
                                &mut faces,
                                Direction::Up,
                                segment.max,
                                Rect {
                                    u0: x0,
                                    v0: z0,
                                    u1: x1,
                                    v1: z1,
                                },
                                segment.material,
                            );
                        }
                        if segment.bottom {
                            add_face(
                                &mut faces,
                                Direction::Down,
                                segment.min,
                                Rect {
                                    u0: x0,
                                    v0: z0,
                                    u1: x1,
                                    v1: z1,
                                },
                                material(segment.span, Direction::Down, centre(z * CELLS + x)),
                            );
                        }
                        for (direction, nx, nz, edge, nw, plane, u0, u1) in [
                            (
                                Direction::West,
                                x as i32 - 1,
                                z as i32,
                                dx == 0,
                                [wx + dx - 1, wz + dz],
                                x0,
                                z0,
                                z1,
                            ),
                            (
                                Direction::East,
                                x as i32 + 1,
                                z as i32,
                                dx + step == stride,
                                [wx + dx + step, wz + dz],
                                x1,
                                z0,
                                z1,
                            ),
                            (
                                Direction::North,
                                x as i32,
                                z as i32 - 1,
                                dz == 0,
                                [wx + dx, wz + dz - 1],
                                z0,
                                x0,
                                x1,
                            ),
                            (
                                Direction::South,
                                x as i32,
                                z as i32 + 1,
                                dz + step == stride,
                                [wx + dx, wz + dz + step],
                                z1,
                                x0,
                                x1,
                            ),
                        ] {
                            // Near meshes may have culled against the real neighbor.
                            // Keep a finite seal where their full-detail area begins:
                            // opaque only, as the near water carries on into the far
                            // water and a seal shows through it as a wall.
                            if is_covered(nw[0], nw[1]) {
                                if segment.material.opaque {
                                    add_side(
                                        &mut faces,
                                        direction,
                                        plane,
                                        u0,
                                        u1,
                                        segment,
                                        &[],
                                        || material(segment.span, direction, centre(z * CELLS + x)),
                                        own_sky,
                                    );
                                }
                            } else if !edge {
                                add_side(
                                    &mut faces,
                                    direction,
                                    plane,
                                    u0,
                                    u1,
                                    segment,
                                    column,
                                    || material(segment.span, direction, centre(z * CELLS + x)),
                                    own_sky,
                                );
                            } else if (0..CELLS as i32).contains(&nx)
                                && (0..CELLS as i32).contains(&nz)
                            {
                                add_side(
                                    &mut faces,
                                    direction,
                                    plane,
                                    u0,
                                    u1,
                                    segment,
                                    &prepared[nz as usize * CELLS + nx as usize],
                                    || material(segment.span, direction, centre(z * CELLS + x)),
                                    own_sky,
                                );
                            } else {
                                let edge_side = match direction {
                                    Direction::West => 0,
                                    Direction::East => 1,
                                    Direction::North => 2,
                                    Direction::South => 3,
                                    _ => unreachable!(),
                                };
                                if neighbors[edge_side].is_empty() {
                                    add_side(
                                        &mut faces,
                                        direction,
                                        plane,
                                        u0,
                                        u1,
                                        segment,
                                        &[],
                                        || material(segment.span, direction, centre(z * CELLS + x)),
                                        own_sky,
                                    );
                                }
                                // A coarse face can meet several finer columns.
                                // Clip each interval independently to avoid missing walls.
                                for (i, edge) in neighbors[edge_side].iter().enumerate() {
                                    let start = u0.max(edge.start as f32);
                                    let end = u1.min(edge.end as f32);
                                    if start < end {
                                        add_side(
                                            &mut faces,
                                            direction,
                                            plane,
                                            start,
                                            end,
                                            segment,
                                            &prepared[edge_offsets[edge_side] + i],
                                            || {
                                                material(
                                                    segment.span,
                                                    direction,
                                                    centre(z * CELLS + x),
                                                )
                                            },
                                            own_sky,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut mesh = SectionMesh::default();
    for (key, mut rects) in faces {
        merge(&mut rects);
        for rect in rects {
            emit(&mut mesh, key, rect)
        }
    }
    mesh.finish_sprite_layers();
    // Far heightmap tiles draw blurred seed colours: their vertices carry the
    // far flag, which terrain.wgsl reads to skip the block texture's stripes,
    // cutout holes and per-pixel shadow work.
    if key.level >= HEIGHTMAP_LEVEL {
        for v in mesh.solid.iter_mut().chain(mesh.translucent.iter_mut()) {
            v.layer |= 0x4000;
        }
    }
    mesh
}

#[cfg(test)]
mod refinement_tests {
    use super::super::Surface;
    use super::*;
    #[test]
    fn fallback_mesh_leaves_exactly_the_fine_tiles_footprints_empty() {
        let parent = TileKey {
            x: -1,
            z: -1,
            level: 2,
        };
        // Smaller than one coarse cell, away from the parent's outer edges.
        let fine = TileKey {
            x: -3,
            z: -3,
            level: 0,
        };
        let columns = vec![
            Column {
                surface: None,
                spans: vec![Span {
                    min: 0,
                    max: 8,
                    state: BlockStateId(1),
                    biome: 0,
                    light: 240,
                }]
                .into()
            };
            256
        ];
        let columns: Vec<&Column> = columns.iter().collect();
        let mesh = mesh_columns_with_edges(
            parent,
            &columns,
            &[],
            &FxHashSet::default(),
            None,
            &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            &[fine],
            crate::chunk::MIN_Y,
            |_, _, _| Material {
                faces: [
                    Some(Paint {
                        layer: 1,
                        light: 240,
                        color: u32::MAX,
                        translucent: false,
                    }),
                    None,
                ],
                fluid: 0,
                opaque: true,
            },
        );
        let [ox, _, oz] = parent.origin();
        let [fx, _, fz] = fine.origin();
        let mut area = 0.0;
        for quad in mesh.solid.as_chunks::<4>().0 {
            if !quad.iter().all(|v| v.position()[1] == 8.0) {
                continue;
            }
            let a = glam::Vec3::from(quad[0].position());
            let b = glam::Vec3::from(quad[1].position());
            let c = glam::Vec3::from(quad[2].position());
            let normal = (b - a).cross(c - a);
            area += normal.y;
            let min_x = quad
                .iter()
                .map(|v| v.position()[0])
                .fold(f32::INFINITY, f32::min)
                + ox as f32;
            let max_x = quad
                .iter()
                .map(|v| v.position()[0])
                .fold(f32::NEG_INFINITY, f32::max)
                + ox as f32;
            let min_z = quad
                .iter()
                .map(|v| v.position()[2])
                .fold(f32::INFINITY, f32::min)
                + oz as f32;
            let max_z = quad
                .iter()
                .map(|v| v.position()[2])
                .fold(f32::NEG_INFINITY, f32::max)
                + oz as f32;
            assert!(
                !(min_x < (fx + 16) as f32
                    && max_x > fx as f32
                    && min_z < (fz + 16) as f32
                    && max_z > fz as f32)
            );
        }
        assert_eq!(area, (parent.size() * parent.size() - 256) as f32);
    }

    #[test]
    fn heightfield_cuts_out_a_finer_tile_footprint() {
        let parent = TileKey {
            x: 0,
            z: 0,
            level: 5,
        };
        let fine = TileKey {
            x: 4,
            z: 4,
            level: 0,
        };
        let columns: Vec<Column> = (0..CELLS * CELLS)
            .map(|_| Column {
                surface: Some(Surface {
                    height: 8,
                    state: BlockStateId(1),
                    water: None,
                    biome: 0,
                }),
                spans: vec![Span {
                    min: 0,
                    max: 8,
                    state: BlockStateId(1),
                    biome: 0,
                    light: 240,
                }]
                .into(),
            })
            .collect();
        let refs: Vec<&Column> = columns.iter().collect();
        let mesh = mesh_columns_with_edges(
            parent,
            &refs,
            &[],
            &FxHashSet::default(),
            None,
            &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            &[fine],
            crate::chunk::MIN_Y,
            |_, _, _| Material {
                faces: [
                    Some(Paint {
                        layer: 1,
                        light: 240,
                        color: u32::MAX,
                        translucent: false,
                    }),
                    None,
                ],
                fluid: 0,
                opaque: true,
            },
        );
        let stride = parent.size() / CELLS as i32;
        let mut area = 0.0;
        for quad in mesh.solid.as_chunks::<4>().0 {
            if !quad.iter().all(|v| v.position()[1] == 8.0) {
                continue;
            }
            let a = glam::Vec3::from(quad[0].position());
            let b = glam::Vec3::from(quad[1].position());
            let c = glam::Vec3::from(quad[2].position());
            area += (b - a).cross(c - a).y;
            let [fx, _, fz] = fine.origin();
            let (x0, x1) = quad
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v.position()[0]), hi.max(v.position()[0]))
                });
            let (z0, z1) = quad
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v.position()[2]), hi.max(v.position()[2]))
                });
            assert!(
                (x1.min((fx + 16) as f32) - x0.max(fx as f32)).max(0.0)
                    * (z1.min((fz + 16) as f32) - z0.max(fz as f32)).max(0.0)
                    == 0.0,
                "heightfield top overlaps the finer tile"
            );
        }
        assert_eq!(
            area,
            (parent.size() * parent.size() - stride * stride) as f32
        );
    }
}

/// The tile's columns with the gaps no open air reaches filled with the
/// material below them, or the fluid above them (kelp and seagrass leave gaps
/// in water): a cave seen from nowhere adds no faces. A gap is open when a
/// neighbouring column's top lies below the gap's top, so that column's open
/// air looks into it, when a gap of the tile's neighbour meets it, or when it
/// meets an open gap of a neighbouring column: the air under a canopy reaches
/// the sky through any clearing, where filling it raised pillars of the floor
/// block. Unknown neighbours, and cells that `whole` rejects (they meet
/// full-detail or finer terrain), count as open; those cells keep their gaps.
fn fill_buried_gaps<'a>(
    columns: &'a [&'a Column],
    neighbors: &'a [Vec<EdgeColumn<'a>>; 4],
    stride: i32,
    whole: impl Fn(usize, usize) -> bool,
    mut fluid: impl FnMut(&Span) -> bool,
) -> Vec<std::borrow::Cow<'a, Column>> {
    use std::borrow::Cow;
    let top = |column: &Column| {
        column
            .spans
            .iter()
            .rev()
            .find(|span| span.min < span.max)
            .map_or(i16::MIN, |span| span.max)
    };
    // Each column's gaps: the index of the span above, the gap's bottom and top.
    let gaps_of = |column: &Column| -> Vec<(usize, i16, i16)> {
        column
            .spans
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[0].max < pair[1].min)
            .map(|(k, pair)| (k + 1, pair[0].max, pair[1].min))
            .collect()
    };
    let meets =
        |(lo, hi): (i16, i16), (other_lo, other_hi): (i16, i16)| lo < other_hi && other_lo < hi;
    let tops: [i16; CELLS * CELLS] = std::array::from_fn(|i| {
        if whole(i % CELLS, i / CELLS) {
            top(columns[i])
        } else {
            i16::MIN
        }
    });
    let gaps: Vec<Vec<(usize, i16, i16)>> = (0..CELLS * CELLS)
        .map(|i| {
            if whole(i % CELLS, i / CELLS) {
                gaps_of(columns[i])
            } else {
                Vec::new()
            }
        })
        .collect();
    let touching = |side: usize, cell: usize| {
        let (start, end) = (cell as i32 * stride, (cell as i32 + 1) * stride);
        neighbors[side]
            .iter()
            .filter(move |edge| edge.start < end && start < edge.end)
    };
    let edge = |side: usize, cell: usize| {
        touching(side, cell)
            .map(|edge| top(edge.column))
            .min()
            .unwrap_or(i16::MIN)
    };
    // A gap of the tile's neighbour may be open through tiles this one can't
    // see: it counts as open.
    let edge_gap = |side: usize, cell: usize, gap: (i16, i16)| {
        touching(side, cell).any(|edge| {
            gaps_of(edge.column)
                .iter()
                .any(|&(_, lo, hi)| meets((lo, hi), gap))
        })
    };
    let around = |i: usize| {
        let (x, z) = (i % CELLS, i / CELLS);
        [
            (x > 0).then(|| i - 1),
            (x + 1 < CELLS).then(|| i + 1),
            (z > 0).then(|| i - CELLS),
            (z + 1 < CELLS).then(|| i + CELLS),
        ]
        .into_iter()
        .flatten()
    };
    let mut open: Vec<Vec<bool>> = gaps.iter().map(|g| vec![false; g.len()]).collect();
    let mut reached = Vec::new();
    for i in 0..CELLS * CELLS {
        let (x, z) = (i % CELLS, i / CELLS);
        let lowest = [
            if x > 0 { tops[i - 1] } else { edge(0, z) },
            if x + 1 < CELLS {
                tops[i + 1]
            } else {
                edge(1, z)
            },
            if z > 0 { tops[i - CELLS] } else { edge(2, x) },
            if z + 1 < CELLS {
                tops[i + CELLS]
            } else {
                edge(3, x)
            },
        ]
        .into_iter()
        .min()
        .unwrap_or(i16::MIN);
        for (g, &(_, lo, hi)) in gaps[i].iter().enumerate() {
            let gap = (lo, hi);
            if hi > lowest
                || (x == 0 && edge_gap(0, z, gap))
                || (x + 1 == CELLS && edge_gap(1, z, gap))
                || (z == 0 && edge_gap(2, x, gap))
                || (z + 1 == CELLS && edge_gap(3, x, gap))
            {
                open[i][g] = true;
                reached.push((i, g));
            }
        }
    }
    while let Some((i, g)) = reached.pop() {
        let (_, lo, hi) = gaps[i][g];
        for j in around(i) {
            for (h, &(_, other_lo, other_hi)) in gaps[j].iter().enumerate() {
                if !open[j][h] && meets((lo, hi), (other_lo, other_hi)) {
                    open[j][h] = true;
                    reached.push((j, h));
                }
            }
        }
    }
    columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let column = *column;
            if open[i].iter().all(|&open| open) {
                return Cow::Borrowed(column);
            }
            let mut filled = Column {
                surface: column.surface,
                spans: Spans::with_capacity(column.spans.len()),
            };
            for (k, &span) in column.spans.iter().enumerate() {
                let mut span = span;
                let buried = gaps[i]
                    .iter()
                    .zip(&open[i])
                    .any(|(&(above, _, _), &open)| above == k && !open);
                if buried && let Some(below) = filled.spans.last_mut() {
                    if fluid(&span) {
                        span.min = below.max;
                    } else {
                        below.max = span.min;
                    }
                }
                filled.push(span);
            }
            Cow::Owned(filled)
        })
        .collect()
}

/// Adds the parts of `segment`'s side not hidden by `neighbor`, the column
/// it faces. Parts above the neighbour's top face open air: reduced columns
/// and the seals at full-detail edges expose sides that were buried (their
/// recorded light is a cave's or none), so those take at least the sky light
/// over the top of the neighbour (or of this column, with no neighbour).
fn add_side(
    faces: &mut Faces,
    direction: Direction,
    plane: f32,
    u0: f32,
    u1: f32,
    segment: &Segment<'_>,
    neighbor: &[Segment<'_>],
    mut side: impl FnMut() -> Material,
    own_sky: u16,
) {
    let (open, sky) = neighbor.last().map_or((f32::NEG_INFINITY, own_sky), |top| {
        (f32::from(top.span.max), top.span.light & 0xff)
    });
    let mut cached = None;
    let mut add = |v0: f32, v1: f32| {
        let side = match cached {
            Some(side) => side,
            None => {
                let side = side();
                cached = Some(side);
                side
            }
        };
        let mut lit = side;
        for paint in lit.faces.iter_mut().flatten() {
            paint.light = (paint.light & 0xff00) | (paint.light & 0xff).max(sky);
        }
        let split = open.clamp(v0, v1);
        for (v0, v1, material) in [(v0, split, side), (split, v1, lit)] {
            if v0 < v1 {
                add_face(faces, direction, plane, Rect { u0, v0, u1, v1 }, material);
            }
        }
    };
    let mut floor = segment.min;
    for next in neighbor {
        if next.max <= floor
            || next.min >= segment.max
            || !hides(next.span, next.material, segment.span, segment.material)
        {
            continue;
        }
        if floor < next.min {
            add(floor, next.min.min(segment.max));
        }
        floor = floor.max(next.max);
        if floor >= segment.max {
            break;
        }
    }
    if floor < segment.max {
        add(floor, segment.max);
    }
}

fn merge(rects: &mut Vec<Rect>) {
    rects.sort_unstable_by(|a, b| {
        a.v0.total_cmp(&b.v0)
            .then(a.v1.total_cmp(&b.v1))
            .then(a.u0.total_cmp(&b.u0))
            .then(a.u1.total_cmp(&b.u1))
    });
    let mut end = 0;
    for i in 0..rects.len() {
        let rect = rects[i];
        if end > 0
            && rects[end - 1].v0 == rect.v0
            && rects[end - 1].v1 == rect.v1
            && rects[end - 1].u1 == rect.u0
        {
            rects[end - 1].u1 = rect.u1;
        } else {
            rects[end] = rect;
            end += 1
        }
    }
    rects.truncate(end);
    rects.sort_unstable_by(|a, b| {
        a.u0.total_cmp(&b.u0)
            .then(a.u1.total_cmp(&b.u1))
            .then(a.v0.total_cmp(&b.v0))
            .then(a.v1.total_cmp(&b.v1))
    });
    end = 0;
    for i in 0..rects.len() {
        let rect = rects[i];
        if end > 0
            && rects[end - 1].u0 == rect.u0
            && rects[end - 1].u1 == rect.u1
            && rects[end - 1].v1 == rect.v0
        {
            rects[end - 1].v1 = rect.v1;
        } else {
            rects[end] = rect;
            end += 1
        }
    }
    rects.truncate(end);
}
/// The widest face `emit` writes, in blocks: its UVs, counted from the face's
/// corner, must stay within `TerrainVertex::tiled`'s range.
const MAX_FACE: f32 = 1024.0;
/// A face that would be more than this many `MAX_FACE` pieces is one quad
/// whose UVs are scaled to fit: a cell of the coarsest tiles is thousands to
/// millions of blocks wide, and the texture repeats within it are far below a
/// pixel.
const MAX_PIECES: f32 = 4.0;
/// Steps along a cell of a tile cut around finer terrain, at most.
const MAX_STEPS: i32 = 32;
fn emit(mesh: &mut SectionMesh, key: FaceKey, rect: Rect) {
    let pieces = ((rect.u1 - rect.u0) / MAX_FACE).ceil() * ((rect.v1 - rect.v0) / MAX_FACE).ceil();
    if pieces > MAX_PIECES {
        let extent = (rect.u1 - rect.u0).max(rect.v1 - rect.v0);
        emit_quad(mesh, key, rect, (MAX_FACE / extent).min(1.0));
        return;
    }
    let mut u0 = rect.u0;
    while u0 < rect.u1 {
        let u1 = rect.u1.min(u0 + MAX_FACE);
        let mut v0 = rect.v0;
        while v0 < rect.v1 {
            let v1 = rect.v1.min(v0 + MAX_FACE);
            emit_quad(mesh, key, Rect { u0, v0, u1, v1 }, 1.0);
            v0 = v1;
        }
        u0 = u1;
    }
}
/// `uv_scale` shrinks the UVs, in blocks from the face's corner, of a face
/// too wide for them.
fn emit_quad(mesh: &mut SectionMesh, key: FaceKey, rect: Rect, uv_scale: f32) {
    mesh.note_sprite_layer(key.layer);
    let Rect { u0, v0, u1, v1 } = rect;
    let p = f32::from_bits(key.plane);
    let mut points = match key.direction {
        Direction::Up | Direction::Down => [[u0, p, v1], [u1, p, v1], [u1, p, v0], [u0, p, v0]],
        Direction::North | Direction::South => [[u0, v1, p], [u1, v1, p], [u1, v0, p], [u0, v0, p]],
        Direction::West | Direction::East => [[p, v1, u1], [p, v1, u0], [p, v0, u0], [p, v0, u1]],
    };
    if matches!(
        key.direction,
        Direction::Down | Direction::South | Direction::East
    ) {
        points.reverse()
    }
    let out = if key.translucent {
        &mut mesh.translucent
    } else {
        &mut mesh.solid
    };
    // Tile origins are integer multiples of a block, so local coordinates
    // retain the world texture phase without distant-coordinate precision loss;
    // a whole number of blocks off each UV keeps it (terrain.wgsl's `tile_uv`
    // repeats the sprite per block) and counts it from the face's corner.
    let uvs = points.map(|pos| match key.direction {
        Direction::Up | Direction::Down => [pos[0], -pos[2]],
        Direction::North | Direction::South => [pos[0], -pos[1]],
        Direction::West | Direction::East => [pos[2], -pos[1]],
    });
    let corner = [0, 1].map(|i| {
        uvs.iter()
            .map(|uv| uv[i])
            .fold(f32::INFINITY, f32::min)
            .floor()
    });
    out.extend(points.into_iter().zip(uvs).map(|(pos, uv)| {
        TerrainVertex::tiled(
            pos,
            [
                (uv[0] - corner[0]) * uv_scale,
                (uv[1] - corner[1]) * uv_scale,
            ],
            key.layer,
            key.color,
            key.light,
        )
    }));
}

#[cfg(test)]
mod tests {
    use super::super::Surface;
    use super::*;
    fn span(min: i16, max: i16, state: u32) -> Span {
        Span {
            min,
            max,
            state: BlockStateId(state),
            biome: 0,
            light: 240,
        }
    }
    fn columns() -> Vec<Column> {
        (0..CELLS * CELLS)
            .map(|_| Column {
                surface: None,
                spans: Spans::new(),
            })
            .collect()
    }
    fn material(span: &Span, _: Direction, _: [i32; 2]) -> Material {
        let water = span.state.0 == 2;
        Material {
            faces: [
                Some(Paint {
                    layer: span.state.0 as u16,
                    light: span.light,
                    color: u32::MAX,
                    translucent: water,
                }),
                None,
            ],
            fluid: u8::from(water),
            opaque: !water,
        }
    }
    fn mesh(columns: &[Column]) -> SectionMesh {
        mesh_columns(
            TileKey {
                x: 0,
                z: 0,
                level: 0,
            },
            columns,
            &FxHashSet::default(),
            None,
            material,
        )
    }
    fn tiled_uv(v: &TerrainVertex) -> [f32; 2] {
        v.uv.map(|u| f32::from(u) / crate::mesher::TILED_UV_STEPS)
    }
    #[test]
    fn wide_faces_split_and_keep_their_texture_phase() {
        let mut mesh = SectionMesh::default();
        let key = FaceKey {
            direction: Direction::Up,
            plane: 64.0f32.to_bits(),
            overlay: 0,
            layer: 1,
            light: 0,
            color: 0,
            translucent: false,
        };
        emit(
            &mut mesh,
            key,
            Rect {
                u0: 0.0,
                v0: 0.5,
                u1: 2500.0,
                v1: 16.0,
            },
        );
        // 2500 blocks: 1024 + 1024 + 452.
        assert_eq!(mesh.solid.len(), 3 * 4);
        let mut area = 0.0;
        for q in mesh.solid.as_chunks::<4>().0 {
            let x = |f: fn(f32, f32) -> f32, i: usize| {
                q.iter().map(|v| v.position()[i]).reduce(f).unwrap()
            };
            area += (x(f32::max, 0) - x(f32::min, 0)) * (x(f32::max, 2) - x(f32::min, 2));
            for v in q {
                // UVs follow the world position modulo a block.
                let uv = tiled_uv(v);
                assert_eq!((uv[0] - v.position()[0]).rem_euclid(1.0), 0.0);
                assert_eq!((uv[1] + v.position()[2]).rem_euclid(1.0), 0.0);
                assert!(uv[0] <= MAX_FACE + 1.0 && uv[1] <= MAX_FACE + 1.0);
            }
        }
        assert_eq!(area, 2500.0 * 15.5);
    }
    #[test]
    fn far_columns_are_heightmaps_of_their_surface_and_water() {
        let fluid = |s: &Span| s.state.0 == 2;
        let strata = Column {
            surface: None,
            spans: vec![span(0, 60, 1), span(60, 63, 3), span(63, 64, 4)].into(),
        };
        let flat = heightmap(&strata, fluid, true, crate::chunk::MIN_Y);
        // The surface's layer stays a lip over the material beneath it, so the
        // cliff shows dirt or stone rather than grass to the world floor.
        assert_eq!(flat.spans, [span(-64, 63, 3), span(63, 64, 4)]);
        // Deeper rings merge the cliff whole (a fraction of a pixel each).
        assert_eq!(
            heightmap(&strata, fluid, false, crate::chunk::MIN_Y).spans,
            [span(-64, 64, 4)],
            "a deep ring's wall is one merged face"
        );
        let sea = Column {
            surface: None,
            spans: vec![span(0, 50, 1), span(50, 62, 2)].into(),
        };
        let flat = heightmap(&sea, fluid, true, crate::chunk::MIN_Y);
        assert_eq!(flat.spans, [span(-64, 50, 1), span(50, 62, 2)]);
        // A tree: a canopy over an air gap is kept where it is. Sinking it to
        // the ground turned each canopy into a leaf pillar from bedrock.
        let tree = Column {
            surface: None,
            spans: vec![span(0, 60, 1), span(60, 64, 4), span(70, 76, 5)].into(),
        };
        assert_eq!(
            heightmap(&tree, fluid, true, crate::chunk::MIN_Y).spans,
            [span(-64, 60, 1), span(60, 64, 4), span(70, 76, 5)]
        );
        assert_eq!(
            heightmap(&tree, fluid, false, crate::chunk::MIN_Y).spans,
            [span(-64, 64, 4), span(70, 76, 5)]
        );
        // Something far above is an island, not a canopy: dropped, not dragged
        // down into a pillar.
        let island = Column {
            surface: None,
            spans: vec![span(0, 60, 1), span(100, 108, 5)].into(),
        };
        assert_eq!(
            heightmap(&island, fluid, true, crate::chunk::MIN_Y).spans,
            [span(-64, 60, 1)]
        );
        // A canopy that does reach the surface stays, so a tree sampled with
        // its own trunk still reads as a tree (see the trunk case below).
        // With the surface recorded (every seed column records it), the trunk
        // is not mistaken for the ground either: the ground ends at the
        // surface, and the trunk and canopy stay above it.
        let trunk = Column {
            surface: Some(crate::lod::Surface {
                height: 64,
                state: BlockStateId(4),
                water: None,
                biome: 0,
            }),
            spans: vec![
                span(0, 60, 1),
                span(60, 64, 4),
                span(64, 70, 6),
                span(70, 76, 5),
            ]
            .into(),
        };
        assert_eq!(
            heightmap(&trunk, fluid, true, crate::chunk::MIN_Y).spans,
            [
                span(-64, 60, 1),
                span(60, 64, 4),
                span(64, 70, 6),
                span(70, 76, 5)
            ]
        );
        assert!(
            heightmap(&columns()[0], fluid, true, crate::chunk::MIN_Y)
                .spans
                .is_empty()
        );
        // A rolling landscape of strata: far tiles need a fraction of the quads.
        let rolling: Vec<Column> = (0..CELLS * CELLS)
            .map(|i| {
                let top = 64 + (i * 7 % 5) as i16;
                Column {
                    surface: None,
                    spans: vec![
                        span(0, top - 4, 1),
                        span(top - 4, top - 1, 3),
                        span(top - 1, top, 4),
                    ]
                    .into(),
                }
            })
            .collect();
        let quads = |level| {
            let key = TileKey { x: 0, z: 0, level };
            let mesh = mesh_columns(key, &rolling, &FxHashSet::default(), None, material);
            mesh.solid.len() / 4
        };
        // `deep` is a ring whose walls are past the lip (`level <= 4`) and merge
        // whole, so the comparison is against the ring that still shows strata.
        let (voxel, height, deep) = (
            quads(HEIGHTMAP_LEVEL - 1),
            quads(HEIGHTMAP_LEVEL),
            quads(HEIGHTMAP_LEVEL + 5),
        );
        // The lip buys a two-material cliff (a face along each step) in the rings
        // that show one; it must still not cost more than the voxel mesh it
        // replaces.
        assert!(height <= voxel, "{height} quads against {voxel}");
        // From the rings whose walls are a fraction of a pixel, the cliff merges
        // whole again: a fraction of the voxel mesh's quads.
        assert!(deep * 3 < voxel * 2, "{deep} quads against {voxel}");
    }
    #[test]
    fn world_sized_faces_are_one_quad_with_uvs_that_fit() {
        let mut mesh = SectionMesh::default();
        let key = FaceKey {
            direction: Direction::Up,
            plane: 64.0f32.to_bits(),
            overlay: 0,
            layer: 1,
            light: 0,
            color: 0,
            translucent: false,
        };
        let size = 4_194_304.0;
        emit(
            &mut mesh,
            key,
            Rect {
                u0: 0.0,
                v0: 0.0,
                u1: size,
                v1: size / 2.0,
            },
        );
        assert_eq!(mesh.solid.len(), 4);
        let corners: Vec<[f32; 3]> = mesh.solid.iter().map(|v| v.position()).collect();
        // World-sized LOD faces use the 128-block position grid; the face is
        // still one quad and its UVs fit.
        assert!(
            corners
                .iter()
                .any(|p| (p[0] - size).abs() <= 128.0 && (p[2] - size / 2.0).abs() <= 128.0),
            "{corners:?}"
        );
        let top = mesh
            .solid
            .iter()
            .map(tiled_uv)
            .fold(0.0f32, |m, uv| m.max(uv[0]).max(uv[1]));
        assert!(top <= MAX_FACE + 1.0, "UV reaches {top}");
        assert!(
            top > MAX_FACE / 4.0,
            "the texture still repeats across the face"
        );
    }
    fn normal(q: &[TerrainVertex; 4]) -> glam::Vec3 {
        let p = |i: usize| glam::Vec3::from(q[i].position());
        (p(1) - p(0)).cross(p(2) - p(0))
    }
    #[test]
    fn flat_solid_cube_merges_to_six_faces_with_block_scaled_uvs() {
        let mut columns = columns();
        for column in &mut columns {
            column.spans = vec![span(0, 16, 1)].into()
        }
        let mesh = mesh(&columns);
        assert_eq!(mesh.solid.len(), 6 * 4);
        assert!(mesh.translucent.is_empty());
        for q in mesh.solid.as_chunks::<4>().0.iter() {
            let center = q
                .iter()
                .map(|v| glam::Vec3::from(v.position()))
                .sum::<glam::Vec3>()
                / 4.0;
            assert!(normal(q).dot(center - glam::Vec3::splat(8.0)) > 0.0);
            assert_eq!(
                q.iter()
                    .map(|v| tiled_uv(v)[0])
                    .fold(f32::NEG_INFINITY, f32::max)
                    - q.iter()
                        .map(|v| tiled_uv(v)[0])
                        .fold(f32::INFINITY, f32::min),
                16.0
            );
            assert_eq!(
                q.iter()
                    .map(|v| tiled_uv(v)[1])
                    .fold(f32::NEG_INFINITY, f32::max)
                    - q.iter()
                        .map(|v| tiled_uv(v)[1])
                        .fold(f32::INFINITY, f32::min),
                16.0
            );
        }
    }
    #[test]
    fn gaps_and_real_bottoms_remain_open_without_floor_curtains() {
        let mut columns = columns();
        columns[0].spans = vec![span(3, 5, 1), span(8, 9, 1)].into();
        let mesh = mesh(&columns);
        assert_eq!(mesh.solid.len(), 12 * 4);
        assert!(
            mesh.solid
                .iter()
                .all(|v| (3.0..=5.0).contains(&v.position()[1])
                    || (8.0..=9.0).contains(&v.position()[1]))
        );
        assert!(
            mesh.solid
                .as_chunks::<4>()
                .0
                .iter()
                .any(|q| q.iter().all(|v| v.position()[1] == 8.0) && normal(q).y < 0.0)
        );
    }
    #[test]
    fn neighbor_hides_only_its_occupied_side_intervals() {
        let mut columns = columns();
        columns[0].spans = vec![span(0, 10, 1)].into();
        columns[1].spans = vec![span(0, 3, 1), span(7, 10, 1)].into();
        let mesh = mesh(&columns);
        let opening: Vec<_> = mesh
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|q| q.iter().all(|v| v.position()[0] == 1.0) && normal(q).x > 0.0)
            .collect();
        assert_eq!(opening.len(), 1);
        assert_eq!(
            opening[0]
                .iter()
                .map(|v| v.position()[1])
                .fold(f32::INFINITY, f32::min),
            3.0
        );
        assert_eq!(
            opening[0]
                .iter()
                .map(|v| v.position()[1])
                .fold(f32::NEG_INFINITY, f32::max),
            7.0
        );
    }
    #[test]
    fn exposed_buried_sides_take_the_sky_light_over_their_neighbor() {
        let mut columns = columns();
        let dark = |min, max| Span {
            light: 0,
            ..span(min, max, 1)
        };
        // A reduced column whose lower blocks were buried (no light), next to
        // a lower column, and to one with a cave below its top.
        columns[0].spans = vec![dark(0, 9), span(9, 10, 1)].into();
        columns[1].spans = vec![span(0, 4, 1)].into();
        columns[CELLS].spans = vec![span(0, 2, 1), span(6, 12, 1)].into();
        let mesh = mesh(&columns);
        let quads = mesh.solid.as_chunks::<4>().0;
        let bounds = |q: &[TerrainVertex; 4]| {
            let ys = q.iter().map(|v| v.position()[1]);
            (
                ys.clone().fold(f32::INFINITY, f32::min),
                ys.fold(0.0, f32::max),
            )
        };
        // Over the lower neighbour: open air, lit like its top.
        let open: Vec<_> = quads
            .iter()
            .filter(|q| {
                q.iter()
                    .all(|v| v.position()[0] == 1.0 && v.position()[2] <= 1.0)
                    && normal(q).x > 0.0
            })
            .collect();
        assert_eq!(open.len(), 1);
        assert_eq!(bounds(open[0]), (4.0, 10.0));
        assert!(open[0].iter().all(|v| v.light == 240));
        // Into the cave: its recorded light.
        let cave: Vec<_> = quads
            .iter()
            .filter(|q| {
                q.iter()
                    .all(|v| v.position()[2] == 1.0 && v.position()[0] <= 1.0)
                    && normal(q).z > 0.0
            })
            .collect();
        assert_eq!(cave.len(), 1);
        assert_eq!(bounds(cave[0]), (2.0, 6.0));
        assert!(cave[0].iter().all(|v| v.light == 0));
    }
    #[test]
    fn air_under_a_canopy_stays_open_through_a_clearing() {
        let column = |spans: Vec<Span>| Column {
            surface: None,
            spans: spans.into(),
        };
        // The tile's neighbours are solid hills: no open air from the sides.
        let hill_column = column(vec![span(0, 20, 1)]);
        let hill = || EdgeColumn {
            start: 0,
            end: CELLS as i32,
            column: &hill_column,
        };
        let neighbors = [vec![hill()], vec![hill()], vec![hill()], vec![hill()]];
        let fill = |columns: &[Column]| -> Vec<Vec<Span>> {
            let columns: Vec<&Column> = columns.iter().collect();
            fill_buried_gaps(&columns, &neighbors, 1, |_, _| true, |_| false)
                .iter()
                .map(|filled| filled.spans.as_slice().to_vec())
                .collect()
        };
        let mut forest = vec![column(vec![span(0, 5, 1), span(10, 14, 3)]); CELLS * CELLS];
        // A closed canopy hides the floor: filling it under there shows nowhere.
        assert!(fill(&forest).iter().all(|spans| spans[0].max == 10));
        // One clearing lets the sky in: the air under every tree stays air,
        // not a pillar of the floor block up to the leaves.
        forest[5 * CELLS + 5] = column(vec![span(0, 5, 1)]);
        let filled = fill(&forest);
        for (filled, column) in filled.iter().zip(&forest) {
            assert_eq!(filled.as_slice(), column.spans.as_slice());
        }
        // A cave two columns wide sealed in rock is still filled.
        let mut rock = vec![column(vec![span(0, 20, 1)]); CELLS * CELLS];
        for cave in [5 * CELLS + 5, 5 * CELLS + 6] {
            rock[cave] = column(vec![span(0, 8, 1), span(12, 20, 1)]);
        }
        assert!(
            fill(&rock)
                .iter()
                .all(|spans| spans.as_slice() == [span(0, 20, 1)])
        );
    }
    #[test]
    fn caves_no_neighbor_sees_into_add_no_faces() {
        let mut columns = columns();
        for column in &mut columns {
            column.spans = vec![span(0, 20, 1)].into();
        }
        let cave = 5 * CELLS + 5;
        columns[cave].spans = vec![span(0, 8, 1), span(12, 20, 1)].into();
        let at = |mesh: &SectionMesh, y: f32| {
            mesh.solid
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|q| q.iter().all(|v| v.position()[1] == y))
                .count()
        };
        // Closed on every side: filled, no floor or ceiling.
        let closed = mesh(&columns);
        assert_eq!((at(&closed, 8.0), at(&closed, 12.0)), (0, 0));
        assert!(
            closed
                .solid
                .iter()
                .all(|v| v.position()[1] == 0.0 || v.position()[1] == 20.0)
        );
        // A lower neighbour's open air looks in: the cave stays.
        columns[cave + 1].spans = vec![span(0, 10, 1)].into();
        let open = mesh(&columns);
        assert_eq!((at(&open, 8.0), at(&open, 12.0)), (1, 1));
        // Kelp leaves a gap in water: water fills it, without inner faces.
        for column in &mut columns {
            column.spans = vec![span(0, 5, 1), span(5, 10, 2)].into();
        }
        columns[cave].spans = vec![span(0, 5, 1), span(5, 7, 2), span(8, 10, 2)].into();
        let water = mesh(&columns);
        assert!(
            water
                .translucent
                .iter()
                .all(|v| v.position()[1] != 7.0 && v.position()[1] != 8.0)
        );
        let uniform = mesh(&vec![columns[0].clone(); CELLS * CELLS]);
        assert_eq!(water.translucent.len(), uniform.translucent.len());
    }
    #[test]
    fn greedy_faces_preserve_material_and_light_boundaries() {
        let mut columns = columns();
        columns[0].spans = vec![span(0, 1, 1)].into();
        columns[1].spans = vec![span(0, 1, 3)].into();
        columns[2].spans = vec![Span {
            light: 16,
            ..span(0, 1, 3)
        }]
        .into();
        let mesh = mesh(&columns);
        let tops: Vec<_> = mesh
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|q| normal(q).y > 0.0)
            .collect();
        assert_eq!(tops.len(), 3);
        assert_eq!(tops.iter().filter(|q| q[0].layer == 3).count(), 2);
        assert_eq!(tops.iter().filter(|q| q[0].light == 16).count(), 1);
        assert!(!mesh.solid.as_chunks::<4>().0.iter().any(|q| {
            normal(q).x != 0.0
                && q.iter()
                    .all(|v| v.position()[0] == 1.0 || v.position()[0] == 2.0)
        }));
    }
    #[test]
    fn water_keeps_ground_surface_and_culls_shared_fluid_faces() {
        let mut columns = columns();
        columns[0].spans = vec![span(0, 3, 1), span(3, 6, 2)].into();
        columns[1].spans = columns[0].spans.clone();
        let mesh = mesh(&columns);
        assert!(
            mesh.solid
                .as_chunks::<4>()
                .0
                .iter()
                .any(|q| q.iter().all(|v| v.position()[1] == 3.0) && normal(q).y > 0.0)
        );
        assert!(
            mesh.translucent
                .as_chunks::<4>()
                .0
                .iter()
                .all(|q| !(q.iter().all(|v| v.position()[0] == 1.0) && normal(q).x != 0.0))
        );
        assert!(mesh.translucent.as_chunks::<4>().0.iter().any(|q| {
            q.iter()
                .all(|v| (v.position()[1] - (6.0 - 1.0 / 9.0)).abs() < 0.07)
                && normal(q).y > 0.0
        }));
    }
    #[test]
    fn coarse_edge_clips_separate_fine_neighbor_heights() {
        let mut columns = columns();
        for column in &mut columns {
            column.spans = vec![span(0, 16, 1)].into();
        }
        let key = TileKey {
            x: -1,
            z: -1,
            level: 1,
        };
        let mut neighbors: [Vec<EdgeColumn<'_>>; 4] = std::array::from_fn(|_| {
            vec![EdgeColumn {
                start: 0,
                end: 32,
                column: &columns[0],
            }]
        });
        // One fine column is lower than both the coarse cell and its sibling.
        // Sampling only the first neighbor would incorrectly hide this wall.
        let low = Column {
            surface: None,
            spans: vec![span(0, 8, 1)].into(),
        };
        neighbors[0] = vec![
            EdgeColumn {
                start: 0,
                end: 1,
                column: &columns[0],
            },
            EdgeColumn {
                start: 1,
                end: 2,
                column: &low,
            },
            EdgeColumn {
                start: 2,
                end: 32,
                column: &columns[0],
            },
        ];
        let columns: Vec<&Column> = columns.iter().collect();
        let mesh = mesh_columns_with_edges(
            key,
            &columns,
            &[],
            &FxHashSet::default(),
            None,
            &neighbors,
            &[],
            crate::chunk::MIN_Y,
            material,
        );
        let walls: Vec<_> = mesh
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|quad| normal(quad).x < 0.0)
            .collect();
        assert_eq!(walls.len(), 1);
        let wall = walls[0];
        assert!(wall.iter().all(|v| v.position()[0] == 0.0
            && (1.0..=2.0).contains(&v.position()[2])
            && (8.0..=16.0).contains(&v.position()[1])));
        assert_eq!(normal(wall).length(), 8.0);
    }
    #[test]
    fn tile_halo_culls_shared_walls_but_keeps_steps_and_gaps() {
        let mut columns = columns();
        for column in &mut columns {
            column.spans = vec![span(0, 16, 1)].into();
        }
        let mut neighbors = columns[..4 * CELLS].to_vec();
        let key = TileKey {
            x: 0,
            z: 0,
            level: 0,
        };
        let covered = FxHashSet::default();
        let mesh = mesh_columns_with_neighbors(key, &columns, &covered, None, &neighbors, material);
        assert_eq!(
            mesh.solid.len(),
            2 * 4,
            "only the top and real bottom remain"
        );
        assert!(
            mesh.solid
                .as_chunks::<4>()
                .0
                .iter()
                .all(|q| normal(q).y != 0.0)
        );

        for column in &mut neighbors[CELLS..2 * CELLS] {
            column.spans = vec![span(0, 8, 1)].into();
        }
        for column in &mut neighbors[..CELLS] {
            column.spans = vec![span(0, 3, 1), span(7, 16, 1)].into();
        }
        let mesh = mesh_columns_with_neighbors(key, &columns, &covered, None, &neighbors, material);
        assert_eq!(mesh.solid.len(), 4 * 4);
        let sides: Vec<_> = mesh
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|q| normal(q).y == 0.0)
            .collect();
        assert_eq!(sides.len(), 2);
        for q in sides {
            let min = q
                .iter()
                .map(|v| v.position()[1])
                .fold(f32::INFINITY, f32::min);
            let max = q
                .iter()
                .map(|v| v.position()[1])
                .fold(f32::NEG_INFINITY, f32::max);
            if normal(q).x > 0.0 {
                assert_eq!((min, max), (8.0, 16.0));
            } else {
                assert!(normal(q).x < 0.0);
                assert_eq!((min, max), (3.0, 7.0));
            }
        }
    }
    #[test]
    fn a_drawn_resident_column_is_clipped_out_of_the_near_tile() {
        let mut columns = columns();
        for column in &mut columns {
            column.spans = vec![span(0, 16, 1)].into();
        }
        // A 32-block tile (2-block cells) with one drawn resident chunk in it:
        // the LOD leaves out exactly that chunk's 16x16 footprint.
        let key = TileKey {
            x: 1,
            z: 1,
            level: 1,
        };
        let covered: FxHashSet<_> = [(2, 3)].into_iter().collect();
        let mesh = mesh_columns(key, &columns, &covered, Some([32, 48, 48, 64]), material);
        let tops: Vec<_> = mesh
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|q| normal(q).y > 0.0)
            .collect();
        let area = tops
            .iter()
            .map(|q| {
                let (x0, x1) = q
                    .iter()
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                        (lo.min(v.position()[0]), hi.max(v.position()[0]))
                    });
                let (z0, z1) = q
                    .iter()
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                        (lo.min(v.position()[2]), hi.max(v.position()[2]))
                    });
                (x1 - x0) * (z1 - z0)
            })
            .sum::<f32>();
        assert_eq!(
            area,
            (32 * 32 - 16 * 16) as f32,
            "the drawn chunk's footprint must be the only terrain left out"
        );
        for q in tops {
            let (x0, x1) = q
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v.position()[0]), hi.max(v.position()[0]))
                });
            let (z0, z1) = q
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v.position()[2]), hi.max(v.position()[2]))
                });
            assert_eq!(
                (x1.min(48.0) - x0.max(32.0)).max(0.0) * (z1.min(64.0) - z0.max(48.0)).max(0.0),
                0.0,
                "an LOD top face still overlaps the drawn resident chunk"
            );
        }
    }
    #[test]
    fn far_heightmap_tiles_carry_the_flat_colour_flag() {
        let build = |level| {
            let mut columns = columns();
            for column in &mut columns {
                column.surface = Some(Surface {
                    height: 3,
                    state: BlockStateId(1),
                    water: None,
                    biome: 0,
                });
                column.spans = vec![span(0, 3, 1)].into();
            }
            mesh_columns(
                TileKey { x: 0, z: 0, level },
                &columns,
                &FxHashSet::default(),
                None,
                material,
            )
        };
        let far = build(HEIGHTMAP_LEVEL);
        let near = build(HEIGHTMAP_LEVEL - 1);
        assert!(!far.solid.is_empty(), "the far tile meshes");
        assert_eq!(
            far.solid.len() % 4,
            0,
            "heightfield emits quads, not triangles"
        );
        assert!(
            far.solid.iter().all(|v| v.layer & 0x4000 != 0),
            "far heightfield vertices request the blurred seed colour"
        );
        assert!(!near.solid.is_empty(), "the near tile meshes");
        assert!(
            near.solid.iter().all(|v| v.layer & 0x4000 == 0),
            "near LOD keeps its block textures"
        );
    }

    /// Each cell's top is one four-corner quad, which the shared index buffer
    /// always splits along its 0-2 diagonal. On a saddle — two high corners
    /// facing each other — that hinge trails over the ridge and folds both
    /// triangles up, so the cell reads as a blob. The contract the mesher owes
    /// the renderer is therefore: in every emitted top quad, vertices 0 and 2
    /// are the closer pair in height, so the split runs along the ridge.
    #[test]
    fn a_cell_hinges_on_the_diagonal_between_its_closest_corners() {
        // A true saddle: inside each cell the two high corners sit on one
        // diagonal and the two low ones on the other, so the two diagonals
        // differ and the choice is not a tie. A ridge along x is not enough:
        // its cells have equal diagonals, which is what made the first version
        // of this test pass either way.
        let mut columns = columns();
        for (i, column) in columns.iter_mut().enumerate() {
            let (x, z) = ((i % CELLS) as i32, (i / CELLS) as i32);
            // Every cell of this surface is a non-tie: its d-b diagonal spans
            // 20 blocks more than its c-a diagonal, so the default split is the
            // worse one in all 256 cells.
            column.surface = Some(Surface {
                height: 64 + 10 * ((x + 2 * z) % 3),
                state: BlockStateId(1),
                water: None,
                biome: 0,
            });
            column.spans = vec![span(0, 3, 1)].into();
        }
        let key = TileKey {
            x: 0,
            z: 0,
            level: 9,
        };
        let mesh = mesh_columns(key, &columns, &FxHashSet::default(), None, material);
        // Top quads are the ones that span a rectangle in x/z; the walls are
        // vertical and have no x/z area.
        let mut checked = 0;
        for quad in mesh.solid.as_chunks::<4>().0 {
            let p: Vec<[f32; 3]> = quad.iter().map(|v| v.position()).collect();
            let area = (p[1][0] - p[0][0]) * (p[3][2] - p[0][2])
                - (p[3][0] - p[0][0]) * (p[1][2] - p[0][2]);
            if area.abs() < 1.0 {
                continue;
            }
            let h: Vec<f32> = p.iter().map(|v| v[1]).collect();
            let diagonal = (h[0] - h[2]).abs();
            let other = (h[1] - h[3]).abs();
            assert!(
                diagonal <= other + f32::EPSILON,
                "the quad is hinged across its ridge: diagonal {diagonal} > other {other} ({h:?})"
            );
            // A rotation of the corners, never a reflection: back-face culling
            // would drop a reversed triangle.
            let cross = |o: [f32; 3], a: [f32; 3], b: [f32; 3]| {
                (a[0] - o[0]) * (b[2] - o[2]) - (a[2] - o[2]) * (b[0] - o[0])
            };
            let (t0, t1) = (cross(p[0], p[1], p[2]), cross(p[0], p[2], p[3]));
            assert!(
                (t0 > 0.0) == (t1 > 0.0),
                "both triangles of a quad must wind the same way"
            );
            checked += 1;
        }
        assert!(checked > 0, "the heightfield emitted no top quads");
    }

    /// A far cell's top is one flat quad, so its edges step between cells
    /// instead of interpolating into a ramp. The shared corner grid turned a
    /// hill smaller than a cell into a smooth slope, which is what made the far
    /// rings read as blobs; the plateau is the cell's lowest corner, so a cliff
    /// stays a cliff. Vertex count must not change, or the fix costs speed.
    #[test]
    fn a_far_cell_top_is_flat_rather_than_interpolated() {
        let key = TileKey {
            x: 0,
            z: 0,
            level: 9,
        };
        assert!(
            key.size() / CELLS as i32 >= HEIGHTFIELD_CELL,
            "this pins the heightfield path"
        );
        // A slope: every corner sits four blocks above its neighbour in x, so an
        // interpolated quad would be visibly sloped inside every cell.
        let mut sloped = columns();
        for (i, column) in sloped.iter_mut().enumerate() {
            let height = 64 + (i % (CELLS + 1)) as i32 * 4;
            column.surface = Some(Surface {
                height,
                state: BlockStateId(1),
                water: None,
                biome: 0,
            });
            column.spans = vec![span(0, 3, 1)].into();
        }
        let mesh = mesh_columns(key, &sloped, &FxHashSet::default(), None, material);
        // Top quads span a rectangle in x/z; walls are vertical and have none.
        let mut checked = 0;
        for quad in mesh.solid.as_chunks::<4>().0 {
            let p: Vec<[f32; 3]> = quad.iter().map(|v| v.position()).collect();
            let area = (p[1][0] - p[0][0]) * (p[3][2] - p[0][2])
                - (p[3][0] - p[0][0]) * (p[1][2] - p[0][2]);
            if area.abs() < 1.0 {
                continue;
            }
            let h: Vec<f32> = p.iter().map(|v| v[1]).collect();
            assert!(
                h.iter().all(|y| (y - h[0]).abs() < f32::EPSILON),
                "a cell top must be one flat plateau, not a ramp: {h:?}"
            );
            checked += 1;
        }
        assert!(checked > 0, "the heightfield emitted no top quads");
    }

    /// A cell is one quad, so a hill inside it could not exist. When the sampler
    /// gives the cell a centre that stands off its corners' plane, the mesher
    /// fans four triangles from it — emitted as two ordinary four-vertex quads,
    /// `(m,a,b,c)` and `(m,c,d,a)`, so the shared index buffer needs no change.
    ///
    /// This pins the three properties that fan must have: it appears only where
    /// the centre is real terrain, it replaces the quad rather than adding to it,
    /// and its triangles all wind as the original two did.
    #[test]
    fn a_cell_centre_becomes_four_triangles_that_keep_the_quad_wound() {
        let key = TileKey {
            x: 0,
            z: 0,
            level: 9,
        };
        let flat: Vec<Option<Surface>> = vec![None; CELLS * CELLS];
        let flat_columns = columns();
        let plain = mesh_columns_centres(key, &flat_columns, &flat, material);

        // No centre, or a centre on the plane: one quad per cell, exactly as
        // before. Nothing may change here, or every far ring grows vertices.
        let on_plane: Vec<Option<Surface>> = (0..CELLS * CELLS)
            .map(|_| {
                Some(Surface {
                    height: 64,
                    state: BlockStateId(1),
                    water: None,
                    biome: 0,
                })
            })
            .collect();
        let plane = mesh_columns_centres(key, &flat_columns, &on_plane, material);
        assert_eq!(
            plane.solid.len(),
            plain.solid.len(),
            "a centre on the corners' plane must not subdivide the cell"
        );

        // A hill inside every cell: each top becomes two quads (8 vertices).
        let mut hill_columns = columns();
        for column in &mut hill_columns {
            column.surface = Some(Surface {
                height: 64,
                state: BlockStateId(1),
                water: None,
                biome: 0,
            });
            column.spans = vec![span(0, 63, 1)].into();
        }
        let hill: Vec<Option<Surface>> = (0..CELLS * CELLS)
            .map(|_| {
                Some(Surface {
                    height: 68,
                    state: BlockStateId(1),
                    water: None,
                    biome: 0,
                })
            })
            .collect();
        let meshed = mesh_columns_centres(key, &hill_columns, &hill, material);
        let tops: Vec<_> = meshed
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|quad| quad.iter().all(|v| v.position()[1] > 63.0))
            .collect();
        let centre_count = tops
            .iter()
            .filter(|quad| quad.iter().any(|v| (v.position()[1] - 68.0).abs() < 0.5))
            .count();
        assert!(
            centre_count >= CELLS * CELLS,
            "every bulging cell must carry a centre vertex, saw {centre_count}"
        );
        // The hill is real: the mesh reaches 68 where the corners stop at 64.
        assert!(
            meshed.solid.iter().any(|v| v.position()[1] > 66.0),
            "the centre height must reach the mesh"
        );
        // Winding: no emitted quad may be reflected relative to its neighbours.
        for quad in meshed.solid.as_chunks::<4>().0 {
            let p: Vec<[f32; 3]> = quad.iter().map(|v| v.position()).collect();
            let cross = |o: [f32; 3], a: [f32; 3], b: [f32; 3]| {
                (a[0] - o[0]) * (b[2] - o[2]) - (a[2] - o[2]) * (b[0] - o[0])
            };
            let (t0, t1) = (cross(p[0], p[1], p[2]), cross(p[0], p[2], p[3]));
            assert!(
                t0 == 0.0 || t1 == 0.0 || (t0 > 0.0) == (t1 > 0.0),
                "a fanned quad's two triangles must wind the same way"
            );
        }
    }

    #[test]
    fn heightfield_keeps_its_cliffs_and_packs_its_uvs() {
        let mut columns = columns();
        for (i, column) in columns.iter_mut().enumerate() {
            // A slope across the tile: the heightfield must drop a wall where
            // two samples differ, or the ring reads as one smooth blob.
            column.surface = Some(Surface {
                height: 64 + (i / CELLS) as i32,
                state: BlockStateId(1),
                water: None,
                biome: 0,
            });
            column.spans = vec![span(0, 3, 1)].into();
        }
        // One of the coarsest rings, whose top spans more than a `u16` of
        // 1/32-block UV steps: its UVs must stay in range, or the sampler
        // caps them and stretches one texel over the whole tile.
        let key = TileKey {
            x: 0,
            z: 0,
            level: 9,
        };
        let mesh = mesh_columns(key, &columns, &FxHashSet::default(), None, material);
        let limit = u16::MAX as f32 / TILED_UV_STEPS;
        // `tiled` clamps to `u16`, so a negative UV does not just overflow: it
        // pins to zero and the whole face samples one texel. Both axes must be
        // non-negative as well as in range.
        assert!(
            mesh.solid.iter().all(|v| {
                let uv = tiled_uv(v);
                uv[0] >= 0.0 && uv[1] >= 0.0 && uv[0] <= limit && uv[1] <= limit
            }),
            "a far tile's UV left its fixed-point range"
        );
        // A ring below the saturating width keeps absolute UVs; those were
        // negated on the v axis, which pinned every top face to one texel row.
        let near = TileKey {
            x: 0,
            z: 0,
            level: 5,
        };
        assert!(
            near.size() as f32 <= limit,
            "this ring is meant to keep absolute UVs"
        );
        let mesh = mesh_columns(near, &columns, &FxHashSet::default(), None, material);
        assert!(
            mesh.solid.iter().all(|v| {
                let uv = tiled_uv(v);
                uv[0] >= 0.0 && uv[1] >= 0.0
            }),
            "an absolute-UV ring negated its v, sampling a single texel row"
        );
        let walls = mesh
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|q| normal(q).y.abs() < 0.01)
            .count();
        assert!(
            walls > 0,
            "a slope across the tile must keep its side walls, not a flat lid"
        );
    }

    #[test]
    fn a_cliff_face_takes_the_block_under_the_surface() {
        // A grass cap (7) over the sub-surface block (9), with a one-block
        // step across the tile. The top is the cap; the step's face must be
        // the block under it — painting it with the cap turned every far
        // cliff into a green slab the height of the hill.
        let mut columns = columns();
        for (i, column) in columns.iter_mut().enumerate() {
            let height = if i / CELLS == 0 { 64 } else { 65 };
            column.surface = Some(Surface {
                height,
                state: BlockStateId(7),
                water: None,
                biome: 0,
            });
            column.spans = vec![span(0, 60, 7), span(60, height as i16, 9)].into();
        }
        let key = TileKey {
            x: 0,
            z: 0,
            level: 5,
        };
        let mesh = mesh_columns(key, &columns, &FxHashSet::default(), None, material);
        let walls: Vec<u16> = mesh
            .solid
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|q| normal(q).y.abs() < 0.01)
            .map(|q| q[0].layer & 0x3fff)
            .collect();
        assert!(!walls.is_empty(), "the step must keep its wall");
        assert!(
            walls.iter().all(|&layer| layer == 9),
            "a cliff face must show the sub-surface block, not the surface cap: {walls:?}"
        );
    }

    #[test]
    fn a_resident_tile_meshes_nothing() {
        let columns = columns();
        // Level 0 is one chunk, so a tile whose chunk is resident has no
        // uncovered part left: every face would be hidden under the real
        // terrain, and meshing it is pure waste.
        let key = TileKey {
            x: -1,
            z: -1,
            level: 0,
        };
        let [ox, _, oz] = key.origin();
        let mut covered = FxHashSet::default();
        covered.insert((ox.div_euclid(16), oz.div_euclid(16)));
        let mesh = mesh_columns(
            key,
            &columns,
            &covered,
            Some([ox, oz, ox + 16, oz + 16]),
            material,
        );
        assert!(mesh.solid.is_empty());
        assert!(mesh.translucent.is_empty());
    }
}
