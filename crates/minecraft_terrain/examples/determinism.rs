//! Generates the same seeded world several times and compares every chunk
//! around a fixed point: multiplayer peers build their terrain themselves, so
//! it must come out identical each time.
//!
//! `cargo run --release -p minecraft_terrain --example determinism -- <root> [seed] [runs] [radius]`
//! where `<root>` is the fetched Minecraft files (`iw4l-artifacts/minecraft-26.3`).
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use minecraft_terrain::{scene::HandcraftedScene, terrain};
use minecraftoss_core::registries::{DataPaths, Registries};

fn checksum(chunk: &minecraftoss_core::Chunk) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let (min_y, height) = (chunk.min_y(), chunk.height());
    for y in min_y..min_y + height {
        for z in 0..16 {
            for x in 0..16 {
                for byte in chunk.block(x, y, z).0.to_le_bytes() {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
    }
    hash
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("root of the Minecraft files"));
    let seed: i64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(12345);
    let runs: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);
    let radius: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);
    let registries = Arc::new(Registries::load(&DataPaths::under(&root)).map_err(anyhow::Error::msg)?);
    let mut all: Vec<BTreeMap<(i32, i32), u64>> = Vec::new();
    let mut first_blocks: Option<Vec<minecraftoss_core::BlockStateId>> = None;
    for run in 0..runs {
        let started = Instant::now();
        let mut stream = terrain::TerrainStream::for_dimension(registries.clone(), seed, 8, terrain::Dimension::Overworld, None)?;
        let mut scene = HandcraftedScene::streamed(stream.states.clone());
        let block = (8, 80, 8);
        if std::env::var_os("PREGEN").is_some() {
            stream.load_around((8.0, 8.0), radius + 1);
        }
        let wanted: Vec<(i32, i32)> = (-radius..=radius).flat_map(|x| (-radius..=radius).map(move |z| (x, z))).collect();
        while wanted.iter().any(|&pos| scene.generated_chunk(pos).is_none()) {
            stream.server_tick(block, &mut scene);
            std::thread::sleep(std::time::Duration::from_millis(16));
            anyhow::ensure!(started.elapsed().as_secs() < 120, "run {run}: chunks never arrived");
        }
        let sums: BTreeMap<(i32, i32), u64> =
            wanted.iter().map(|&pos| (pos, checksum(scene.generated_chunk(pos).unwrap()))).collect();
        println!("run {run}: {} chunks in {:.1}s", sums.len(), started.elapsed().as_secs_f64());
        // Every block of one chunk, to see what differs.
        let probe = scene.generated_chunk((2, -2)).unwrap();
        let mut blocks = Vec::new();
        for y in probe.min_y()..probe.min_y() + probe.height() {
            for z in 0..16 {
                for x in 0..16 {
                    blocks.push(probe.block(x, y, z));
                }
            }
        }
        if let Some(first) = &first_blocks {
            let mut kinds: BTreeMap<(String, String), usize> = BTreeMap::new();
            let mut ys = (i32::MAX, i32::MIN);
            for (i, (a, b)) in first.iter().zip(&blocks).enumerate() {
                if a != b {
                    let name = |s: minecraftoss_core::BlockStateId| registries.blocks.block(registries.blocks.state(s).block).name.to_string();
                    *kinds.entry((name(*a), name(*b))).or_default() += 1;
                    let y = probe.min_y() + (i / 256) as i32;
                    ys = (ys.0.min(y), ys.1.max(y));
                }
            }
            let mut kinds: Vec<_> = kinds.into_iter().collect();
            kinds.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            println!("  chunk (2,-2) vs run 0: y {ys:?}; {:?}", &kinds[..kinds.len().min(12)]);
        } else {
            first_blocks = Some(blocks);
        }
        all.push(sums);
    }
    let mut differing = 0;
    for pos in all[0].keys() {
        let values: Vec<u64> = all.iter().map(|sums| sums[pos]).collect();
        if values.iter().any(|v| *v != values[0]) {
            differing += 1;
            println!("chunk {pos:?} differs: {values:016x?}");
        }
    }
    println!("{differing} of {} chunks differ across {runs} runs (seed {seed})", all[0].len());
    Ok(())
}
