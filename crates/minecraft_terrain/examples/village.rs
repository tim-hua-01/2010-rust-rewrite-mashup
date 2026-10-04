//! Prints the biggest village near the spawn of each seed.
//!
//! `cargo run --release -p minecraft_terrain --example village -- <root> <range> <seed>...`
//! where `<root>` is the fetched Minecraft files (`iw4l-artifacts/minecraft-26.3`).
use std::sync::Arc;
use std::time::Instant;

use minecraft_terrain::terrain;
use minecraftoss_core::registries::{DataPaths, Registries};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("root of the Minecraft files"));
    let range: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1024);
    let registries = Arc::new(Registries::load(&DataPaths::under(&root)).map_err(anyhow::Error::msg)?);
    for seed in args.filter_map(|s| s.parse::<i64>().ok()) {
        let stream = terrain::TerrainStream::new(registries.clone(), seed, 4)?;
        let started = Instant::now();
        let village = stream.biggest_village(range);
        println!("seed {seed}: spawn {:?}, {village:?} in {:.2}s", stream.world_spawn, started.elapsed().as_secs_f64());
    }
    Ok(())
}
