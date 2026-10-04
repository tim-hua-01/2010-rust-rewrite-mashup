//! Saves of the Minecraft world, under `iw4l-artifacts/minecraft-saves/<name>`:
//! the region files MinecraftOSS keeps the chunks in (the player's edits
//! included, as vanilla stores them) and `meta.json` with what lives outside
//! them: the seed, the time of day, the difficulty, and where the player
//! stood with what in their inventory. Mobs are not saved; the world spawns
//! them again.
//!
//! A loaded save is played in a copy, so the save itself only changes when
//! it is saved again.
use std::path::{Path, PathBuf};

use minecraftoss_player::inventory::ItemStack;
use serde::{Deserialize, Serialize};

const META: &str = "meta.json";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub(crate) struct SaveMeta {
    pub version: u32,
    pub minecraft_version: String,
    pub seed: i64,
    /// `DayCycle::ticks`, unwrapped.
    pub day_ticks: f64,
    pub difficulty: String,
    /// The player's feet in block space.
    pub feet: [f64; 3],
    pub selected: usize,
    pub slots: Vec<Option<SavedStack>>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SavedStack {
    pub id: String,
    pub count: u8,
    pub max: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<serde_json::Value>,
}

impl SavedStack {
    pub fn of(stack: &ItemStack) -> Self {
        Self {
            id: stack.id.clone(),
            count: stack.count,
            max: stack.max,
            components: stack.components.clone(),
        }
    }

    pub fn stack(&self) -> ItemStack {
        let mut stack = ItemStack::new(self.id.clone(), self.count);
        stack.max = self.max;
        stack.components = self.components.clone();
        stack
    }
}

impl SaveMeta {
    pub fn new(seed: i64, day_ticks: f64, difficulty: minecraftoss_player::Difficulty, feet: [f64; 3]) -> Self {
        Self {
            version: VERSION,
            minecraft_version: "26.3".to_owned(),
            seed,
            day_ticks,
            difficulty: format!("{difficulty:?}").to_ascii_lowercase(),
            feet,
            selected: 0,
            slots: Vec::new(),
        }
    }
}

pub(crate) fn saves_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_default()
        .join("iw4l-artifacts")
        .join("minecraft-saves")
}

/// Letters, digits, `-` and `_`: a save name is a directory name.
fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!("`{name}` is not a save name: use letters, digits, - and _"))
    }
}

/// Writes the save: a fresh copy of the region files in `world`, then the
/// meta, swapped in for any older save of that name. Returns the number of
/// region files.
pub(crate) fn write(name: &str, world: &Path, meta: &SaveMeta) -> Result<usize, String> {
    check_name(name)?;
    let dir = saves_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let staging = dir.join(format!(".{name}.saving"));
    let _ = std::fs::remove_dir_all(&staging);
    let regions = copy_dir(world, &staging)?;
    let json = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(staging.join(META), json).map_err(|e| format!("writing {META}: {e}"))?;
    let target = dir.join(name);
    let old = dir.join(format!(".{name}.old"));
    let _ = std::fs::remove_dir_all(&old);
    if target.exists() {
        std::fs::rename(&target, &old).map_err(|e| format!("replacing {}: {e}", target.display()))?;
    }
    std::fs::rename(&staging, &target).map_err(|e| format!("writing {}: {e}", target.display()))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(regions)
}

/// The named save's meta, and a fresh copy of its region files to play in.
pub(crate) fn prepare_load(name: &str) -> Result<(SaveMeta, PathBuf), String> {
    check_name(name)?;
    let save = saves_dir().join(name);
    let json = std::fs::read_to_string(save.join(META))
        .map_err(|_| format!("no save named `{name}` (mc_saves lists them)"))?;
    let meta: SaveMeta = serde_json::from_str(&json).map_err(|e| format!("{name}/{META}: {e}"))?;
    if meta.version > VERSION {
        return Err(format!("save `{name}` is from a newer version ({})", meta.version));
    }
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let playing = std::env::temp_dir().join(format!("iw4l-minecraft-{}-{n}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&playing);
    copy_dir(&save, &playing)?;
    let _ = std::fs::remove_file(playing.join(META));
    Ok((meta, playing))
}

/// The saves there are, by name.
pub(crate) fn list() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(saves_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join(META).is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Copies a directory tree; returns how many `.mca` region files it held.
fn copy_dir(from: &Path, to: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(to).map_err(|e| format!("creating {}: {e}", to.display()))?;
    let mut regions = 0;
    let entries = std::fs::read_dir(from).map_err(|e| format!("reading {}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            regions += copy_dir(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)
                .map_err(|e| format!("copying {}: {e}", source.display()))?;
            regions += usize::from(source.extension().is_some_and(|ext| ext == "mca"));
        }
    }
    Ok(regions)
}
