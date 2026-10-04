//! A Minecraft replica of the MW2 map a Minecraft match stands on: its
//! collision geometry voxelized into blocks at the voxel world's own scale
//! (`sim::voxel::BLOCK` map units a block, map Z up), so the map's spawn
//! points and the replica line up. Brushes become solid blocks, the terrain
//! mesh a surface filled down to the floor; each surface's MW2 type picks the
//! block (concrete, wood, metal, glass...). Clip-only and sky volumes are left
//! out. The host builds it; clients receive the chunks like any terrain.
use std::collections::HashMap;

use minecraft_terrain::terrain::TerrainStream;
use minecraftoss_core::{BlockStateId, Chunk};

const BLOCK: f64 = sim::voxel::BLOCK as f64;
/// `CONTENTS_SOLID`.
const SOLID: u32 = 1;
/// `SURF_SKY` and `SURF_NODRAW`: faces nobody sees.
const SKY: u32 = 0x4;
const NODRAW: u32 = 0x80;

/// The map's geometry in blocks: each solid block's block name, and the
/// floor every column is filled up to.
pub(crate) struct ReplicaVoxels {
    pub blocks: HashMap<(i32, i32, i32), &'static str>,
    pub floor: i32,
    /// The map's half width in blocks around the origin (for the border).
    pub half_width: f64,
}

/// The MW2 surface type in a surface's flags (`SURF_TYPE`).
fn surface_type(flags: u32) -> usize {
    ((flags >> 20) & 0x1f) as usize
}

/// The block an MW2 surface type becomes.
fn material(surface_flags: u32) -> &'static str {
    match surface_type(surface_flags) {
        1 => "minecraft:oak_log",
        2 => "minecraft:bricks",
        3 => "minecraft:red_wool",
        4 => "minecraft:white_wool",
        5 => "minecraft:light_gray_concrete",
        6 | 14 => "minecraft:dirt",
        8 => "minecraft:oak_leaves",
        9 => "minecraft:glass",
        10 => "minecraft:grass_block",
        11 => "minecraft:gravel",
        12 => "minecraft:packed_ice",
        13 => "minecraft:iron_block",
        16 => "minecraft:white_concrete",
        17 => "minecraft:stone_bricks",
        18 => "minecraft:sand",
        19 | 30 => "minecraft:snow_block",
        21 => "minecraft:oak_planks",
        22 => "minecraft:gray_concrete",
        23 => "minecraft:white_terracotta",
        24..=26 => "minecraft:black_wool",
        28 => "minecraft:cyan_terracotta",
        _ => "minecraft:stone_bricks",
    }
}

/// The block an MW2 ground surface becomes: diggable, unlike buildings.
fn ground_material(surface_flags: u32) -> &'static str {
    match surface_type(surface_flags) {
        3 => "minecraft:red_wool",
        6 | 14 => "minecraft:dirt",
        8 | 10 => "minecraft:grass_block",
        11 => "minecraft:gravel",
        18 => "minecraft:sand",
        19 | 30 => "minecraft:snow_block",
        21 => "minecraft:oak_planks",
        _ => "minecraft:andesite",
    }
}

/// Blocks replicas build their buildings from: much harder to shoot or blast
/// through than the ground (`minecraft_mining` scales them).
pub(crate) const FORTIFIED: [&str; 8] = [
    "light_gray_concrete",
    "white_concrete",
    "gray_concrete",
    "bricks",
    "stone_bricks",
    "white_terracotta",
    "cyan_terracotta",
    "iron_block",
];

/// Diggable layers under a ground surface before bedrock.
const GROUND_DEPTH: i32 = 2;

/// A map point in block space (`sim::voxel::to_block`).
fn to_block(origin: [f64; 3], p: [f64; 3]) -> [f64; 3] {
    [origin[0] + p[0] / BLOCK, origin[1] + p[2] / BLOCK, origin[2] - p[1] / BLOCK]
}

/// A block's centre (plus an offset in blocks) in map units.
fn to_map(origin: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [(b[0] - origin[0]) * BLOCK, -(b[2] - origin[2]) * BLOCK, (b[1] - origin[1]) * BLOCK]
}

fn inside(planes: &[[f32; 4]], p: [f64; 3]) -> bool {
    planes
        .iter()
        .all(|n| f64::from(n[0]) * p[0] + f64::from(n[1]) * p[1] + f64::from(n[2]) * p[2] - f64::from(n[3]) <= 0.0)
}

/// The brush's map-space bounds from its axial planes.
fn bounds(planes: &[[f32; 4]]) -> Option<([f64; 3], [f64; 3])> {
    let mut min = [f64::NEG_INFINITY; 3];
    let mut max = [f64::INFINITY; 3];
    for n in planes {
        for axis in 0..3 {
            let others = (0..3).filter(|&a| a != axis).all(|a| n[a].abs() < 1e-4);
            if others && n[axis] > 0.99 {
                max[axis] = max[axis].min(f64::from(n[3]));
            } else if others && n[axis] < -0.99 {
                min[axis] = min[axis].max(-f64::from(n[3]));
            }
        }
    }
    (min.iter().all(|v| v.is_finite()) && max.iter().all(|v| v.is_finite())).then_some((min, max))
}

/// The most blocks a replica reaches from its centre.
const MAX_HALF_WIDTH: f64 = 128.0;

/// How far the map's buildings reach from the origin, in blocks: where most
/// brushes are (the map's far scenery and outer shell don't count).
fn playable_half_width(content: &sim::SimContent, origin: [f64; 3]) -> f64 {
    let mut reach: Vec<f64> = content
        .clip_brushes()
        .iter()
        .filter(|brush| brush.contents & SOLID != 0)
        .filter_map(|brush| bounds(&brush.planes))
        .filter(|(min, max)| (0..3).all(|a| max[a] - min[a] < 12_000.0))
        .map(|(min, max)| {
            let centre = to_block(origin, [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5, (min[2] + max[2]) * 0.5]);
            (centre[0] - origin[0]).abs().max((centre[2] - origin[2]).abs())
        })
        .collect();
    if reach.is_empty() {
        return 32.0;
    }
    reach.sort_by(f64::total_cmp);
    (reach[reach.len() * 95 / 100] + 8.0).min(MAX_HALF_WIDTH)
}

pub(crate) fn voxelize(content: &sim::SimContent, origin: [f64; 3]) -> ReplicaVoxels {
    let mut blocks: HashMap<(i32, i32, i32), &'static str> = HashMap::new();
    let mut lowest = f64::MAX;
    let half_width = playable_half_width(content, origin);
    let within = |x: i32, z: i32| {
        (f64::from(x) + 0.5 - origin[0]).abs() <= half_width && (f64::from(z) + 0.5 - origin[2]).abs() <= half_width
    };
    let started = std::time::Instant::now();
    // Brushes: solid volumes. A thin one (under a block) counts wherever any
    // of 27 samples lands inside, so walls survive; a thick one needs the
    // block's centre, so walls don't thicken into doorways.
    for brush in content.clip_brushes() {
        if brush.contents & SOLID == 0 {
            continue;
        }
        let seen: Vec<u32> = brush.plane_surface_flags.iter().copied().filter(|f| f & (SKY | NODRAW) == 0).collect();
        if seen.is_empty() {
            continue;
        }
        let Some((min, max)) = bounds(&brush.planes) else {
            continue;
        };
        // Unreasonably large volumes are the map's outer shell.
        if (0..3).any(|a| max[a] - min[a] > 12_000.0) {
            continue;
        }
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for flags in &seen {
            *counts.entry(surface_type(*flags)).or_default() += 1;
        }
        let kind = counts.into_iter().max_by_key(|(kind, n)| (*n, usize::from(*kind != 0))).map_or(0, |(k, _)| k);
        let block = material((kind as u32) << 20);
        let thin = (0..3).map(|a| max[a] - min[a]).fold(f64::MAX, f64::min) < BLOCK;
        let lo = to_block(origin, [min[0], max[1], min[2]]);
        let hi = to_block(origin, [max[0], min[1], max[2]]);
        let (x0, x1) = ((lo[0].floor() as i32).max((origin[0] - half_width).floor() as i32), (hi[0].floor() as i32).min((origin[0] + half_width).floor() as i32));
        let (z0, z1) = ((lo[2].floor() as i32).max((origin[2] - half_width).floor() as i32), (hi[2].floor() as i32).min((origin[2] + half_width).floor() as i32));
        if x0 > x1 || z0 > z1 {
            continue;
        }
        lowest = lowest.min(lo[1]);
        for x in x0..=x1 {
            for y in lo[1].floor() as i32..=hi[1].floor() as i32 {
                for z in z0..=z1 {
                    let centre = [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5];
                    let hit = if thin {
                        (0..27).any(|i| {
                            let o = [f64::from(i % 3) / 3.0 + 1.0 / 6.0, f64::from(i / 3 % 3) / 3.0 + 1.0 / 6.0, f64::from(i / 9) / 3.0 + 1.0 / 6.0];
                            inside(&brush.planes, to_map(origin, [f64::from(x) + o[0], f64::from(y) + o[1], f64::from(z) + o[2]]))
                        })
                    } else {
                        inside(&brush.planes, to_map(origin, centre))
                    };
                    if hit {
                        blocks.insert((x, y, z), block);
                    }
                }
            }
        }
    }
    let brushes_ms = started.elapsed().as_millis();
    // The terrain mesh: its surface, sampled every third of a block, and
    // under each upward-facing piece the ground filled down to the floor.
    let tables = &content.clip_mesh().tables;
    let mut ground_tops: HashMap<(i32, i32), (i32, &'static str)> = HashMap::new();
    for (t, tri) in tables.tri_indices.chunks_exact(3).enumerate() {
        if tables.tri_content_flags.get(t).is_some_and(|c| c & SOLID == 0) {
            continue;
        }
        let flags = tables.tri_surface_flags.get(t).copied().unwrap_or(0);
        if flags & (SKY | NODRAW) != 0 {
            continue;
        }
        let v = |i: u16| tables.verts.get(usize::from(i)).map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]);
        let (Some(a), Some(b), Some(c)) = (v(tri[0]), v(tri[1]), v(tri[2])) else {
            continue;
        };
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = [ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0]];
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if length < 1e-6 {
            continue;
        }
        let up = normal[2] / length > 0.6;
        // Pieces wholly outside the arena are scenery.
        let corners = [a, b, c].map(|p| to_block(origin, p));
        if corners.iter().all(|q| !within(q[0].floor() as i32, q[2].floor() as i32)) {
            continue;
        }
        let block = if up { ground_material(flags) } else { material(flags) };
        let longest = [ab, ac, [c[0] - b[0], c[1] - b[1], c[2] - b[2]]]
            .iter()
            .map(|e| (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt())
            .fold(0.0, f64::max);
        let steps = ((longest / (BLOCK / 3.0)).ceil() as usize).clamp(1, 160);
        for i in 0..=steps {
            for j in 0..=steps - i {
                let (u, w) = (i as f64 / steps as f64, j as f64 / steps as f64);
                let p = [a[0] + ab[0] * u + ac[0] * w, a[1] + ab[1] * u + ac[1] * w, a[2] + ab[2] * u + ac[2] * w];
                let q = to_block(origin, p);
                let cell = (q[0].floor() as i32, q[1].floor() as i32, q[2].floor() as i32);
                if !within(cell.0, cell.2) {
                    continue;
                }
                lowest = lowest.min(q[1]);
                if up {
                    let top = ground_tops.entry((cell.0, cell.2)).or_insert((cell.1, block));
                    if cell.1 > top.0 {
                        *top = (cell.1, block);
                    }
                } else {
                    blocks.entry(cell).or_insert(block);
                }
            }
        }
    }
    let floor = if lowest.is_finite() { lowest.floor() as i32 - 1 } else { origin[1].floor() as i32 - 4 };
    // Ground: its surface block over a couple of diggable layers, then
    // bedrock, so blasts make craters but nobody tunnels under the map.
    for ((x, z), (top, block)) in ground_tops {
        blocks.insert((x, top, z), block);
        let fill = if block == "minecraft:grass_block" { "minecraft:dirt" } else { block };
        for y in floor..top {
            let layer = if y >= top - GROUND_DEPTH { fill } else { "minecraft:bedrock" };
            blocks.entry((x, y, z)).or_insert(layer);
        }
    }
    diag::info!(
        World,
        "Minecraft replica voxels: half width {half_width:.0}, brushes {brushes_ms} ms, total {} ms",
        started.elapsed().as_millis()
    );
    ReplicaVoxels { blocks, floor, half_width }
}

/// One arena chunk of the replica over a generated chunk (biomes kept):
/// bedrock up to the floor, grass on it, then the map.
pub(crate) fn build_chunk(stream: &TerrainStream, base: &Chunk, voxels: &ReplicaVoxels) -> Chunk {
    let registries = stream.states.registries();
    let mut ids: HashMap<&'static str, BlockStateId> = HashMap::new();
    let mut state = |name: &'static str| {
        *ids.entry(name).or_insert_with(|| {
            registries
                .blocks
                .block_by_name(name)
                .map_or(BlockStateId::AIR, |id| registries.blocks.block(id).default_state())
        })
    };
    let (bedrock, grass) = (state("minecraft:bedrock"), state("minecraft:grass_block"));
    let mut chunk = base.clone();
    chunk.block_entities = Default::default();
    chunk.generation.entities.clear();
    chunk.light = None;
    let (min_y, height) = (chunk.min_y(), chunk.height());
    let (cx, cz) = (chunk.pos.x * 16, chunk.pos.z * 16);
    for y in min_y..min_y + height {
        for z in 0..16 {
            for x in 0..16 {
                let block = match voxels.blocks.get(&(cx + x as i32, y, cz + z as i32)) {
                    Some(name) => state(name),
                    None if y < voxels.floor => bedrock,
                    None if y == voxels.floor => grass,
                    None => BlockStateId::AIR,
                };
                chunk.set_block_raw(x, y, z, block, registries);
            }
        }
    }
    let kinds = chunk.status.heightmaps();
    chunk.prime_heightmaps(kinds, registries);
    chunk
}
