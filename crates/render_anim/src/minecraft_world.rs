//! The Minecraft map at run time: MinecraftOSS generates and streams a seeded
//! world around the local player, whose chunks become block collision and
//! whose section meshes go to the renderer. The world sits with its player
//! spawn at map origin, a block to `sim::voxel::BLOCK` map units.
use std::collections::HashMap;
use std::sync::{Arc, mpsc};

use bevy::input::gamepad::GamepadButton;
use bevy::prelude::*;
use minecraft_terrain::clouds::CloudMask;
use minecraft_terrain::day_cycle::{DayCycle, Skybox};
use minecraft_terrain::environment::{DimensionEnvironment, View};
use minecraft_terrain::lighting::SkyLight;
use minecraft_terrain::mesh::{Atlas, SectionMesh, Vertex};
use minecraft_terrain::pack::PackStack;
use minecraft_terrain::scene::HandcraftedScene;
use minecraft_terrain::sections::{CullCamera, SectionPos};
use minecraft_terrain::terrain::{Dimension, TerrainStream};
use minecraftoss_core::BlockStateId;
use minecraftoss_core::registries::{DataPaths, Registries};

const VIEW_DISTANCE: i32 = 8;
const TICK_SECONDS: f64 = 1.0 / 20.0;
/// Blocks on a side of the light volume MW2 models are lit from.
pub const LIGHT_VOLUME: i32 = 64;
/// Chunk sections fade in over this long, as the viewer's default option.
const FADE_MILLIS: u64 = 750;

/// What the renderer takes from the world each frame.
#[derive(Resource, Default)]
pub struct MinecraftWorldView {
    /// A Minecraft map is loaded: the stand-in map's world is not drawn.
    pub active: bool,
    /// Block point at map origin.
    pub origin: [f64; 3],
    pub atlas: Option<Arc<Atlas>>,
    pub uploads: Vec<(SectionPos, SectionMesh)>,
    pub removed: Vec<SectionPos>,
    pub visible: Vec<(SectionPos, f32)>,
    /// Bumped when the world is replaced, so stale sections are dropped.
    pub generation: u64,
    /// The environment uniform of MinecraftOSS for this frame, in block space.
    pub environment: [[f32; 4]; 16],
    /// Sun and the eight moon phases, 32 pixels each, side by side.
    pub celestial: Option<Arc<image::RgbaImage>>,
    pub clouds: Option<Arc<(Vec<Vertex>, Vec<u32>)>>,
    /// Sky and block light around the player: origin block, then
    /// `LIGHT_VOLUME` cubed pairs, x fastest then z then y.
    pub light_volume: Option<Arc<([i32; 3], Vec<u8>)>>,
    /// Sky and block light at the eye, for the view model.
    pub eye_light: [f32; 2],
    /// Break particles as section vertices and indices, rebuilt each frame.
    pub particles: (Vec<u8>, Vec<u32>),
    /// Destroy stage cubes over blocks being mined: position, strip uv.
    pub cracks: (Vec<[f32; 5]>, Vec<u32>),
    /// The ten destroy stages side by side.
    pub crack_texture: Option<Arc<image::RgbaImage>>,
    /// Mob models (cut out, back-face culled, translucent) and entity
    /// shadows, as `mesh::Vertex` bytes and indices.
    pub entity_meshes: [(Vec<u8>, Vec<u32>); 4],
    /// The black card behind the inventory's character, in blocks.
    pub backdrop: Option<[[f32; 3]; 4]>,
    /// The first-person hand or held item: section vertex bytes in view
    /// space (x right, y up, z back) and indices, with the projection that
    /// draws them (vanilla's fixed 70 degree hand field of view).
    pub hand: (Vec<u8>, Vec<u32>),
    pub hand_clip: [f32; 16],
}

struct Loaded {
    stream: TerrainStream,
    scene: HandcraftedScene,
    packs: PackStack,
    atlas: Arc<Atlas>,
    registries: Arc<Registries>,
    seed: i64,
    environment: DimensionEnvironment,
    celestial: Arc<image::RgbaImage>,
    cloud_mask: Option<CloudMask>,
    crack_texture: Arc<image::RgbaImage>,
    /// A replica's or village's half width in blocks, for its border.
    arena_half_width: Option<f64>,
}

/// The hand's swing and the timers of mining and placing by hand.
#[derive(Default)]
struct HandState {
    /// Ticks into a swing, while one runs.
    swing: Option<f32>,
    /// Seconds towards the next hand-mining and placing tick.
    clock: f64,
    /// Ticks until another placement while the button is held
    /// (`rightClickDelay`).
    place_delay: u32,
}

/// The player's walk, for vanilla's step and fall sounds.
#[derive(Default)]
struct StepState {
    last: Option<[f64; 3]>,
    /// `Entity.moveDist` and `nextStep`.
    move_dist: f32,
    next_step: f32,
    /// The highest point since leaving the ground.
    air_peak: Option<f64>,
}

#[derive(Default)]
struct Runtime {
    loading: Option<mpsc::Receiver<Result<Loaded, String>>>,
    /// The installing match's difficulty: Game Setup's `scr_mc_difficulty`,
    /// else `IW4L_MINECRAFT_DIFFICULTY`.
    difficulty: Option<minecraftoss_player::Difficulty>,
    /// A save `mc_load` prepared, for the next Minecraft match to install:
    /// its meta and the copy of its region files to play in.
    pending_load: Option<(crate::minecraft_saves::SaveMeta, std::path::PathBuf)>,
    /// The loading save's meta, applied once its world is in.
    restore: Option<crate::minecraft_saves::SaveMeta>,
    /// The player's feet in block space, as of the last frame.
    last_feet: Option<[f64; 3]>,
    /// A client waits for the host's world settings (`mc_*` server info)
    /// before it can load the world.
    awaiting_settings: bool,
    /// The match's world settings once known: chosen on the host, received
    /// on a client.
    settings: Option<WorldSettings>,
    /// The spawn's ground is loaded and in collision: the player may join.
    ready: bool,
    /// The spawn chunk's block-state checksum, once generated.
    spawn_check: Option<u64>,
    /// On a client: the host's arena chunks arriving in pieces (generation,
    /// pieces by column), and whole ones waiting for the world to load.
    chunk_parts: (u32, HashMap<[i32; 2], Vec<Option<std::sync::Arc<[u8]>>>>),
    chunks_waiting: Vec<Vec<u8>>,
    /// On the host: the next arena chunk to encode for clients.
    terrain_next: usize,
    /// On a client: the host's edits waiting to be applied (first sequence,
    /// edits), and the next live sequence expected.
    edits_waiting: Vec<(u32, Vec<frame::McEdit>)>,
    next_edit: (u32, u32),
    /// The building kit was topped up for this life.
    kit_given: bool,
    /// A scripted right click (`mc_use`) waiting for the hand.
    scripted_use: bool,
    /// The heart sprites (`hearts_image`), once the world's packs are in.
    hearts: Option<Handle<Image>>,
    world: Option<Loaded>,
    day: DayCycle,
    environment_accumulator: f64,
    environment_primed: bool,
    light: Option<SkyLight>,
    light_volume_at: Option<[i32; 3]>,
    light_volume_age: u32,
    cloud_center: Option<(i32, i32)>,
    mining: crate::minecraft_mining::Mining,
    minimap: crate::minecraft_minimap::Minimap,
    hand: HandState,
    steps: StepState,
    sounds: Option<crate::minecraft_sounds::Sounds>,
    inventory_ui: crate::minecraft_inventory::InventoryUi,
    entities: Option<crate::minecraft_entities::Entities>,
    /// Shape id of each block state already seen.
    shapes: HashMap<BlockStateId, u16>,
    /// Boxes of each shape id, to reuse an id for a repeated shape.
    shape_ids: HashMap<Vec<[u32; 6]>, u16>,
    /// Players the host has placed on the Minecraft spawn this life.
    spawned: std::collections::HashSet<sim::ClientId>,
    /// How many times the host has spawned each player, to vary the spot.
    spawn_counts: HashMap<sim::ClientId, u32>,
}

/// The player's MW2 body stands in the inventory's character window, on a
/// black card: placed from this frame's camera and projection where the
/// window shows, facing the camera, turned and aiming towards the mouse,
/// and drawn with the card in the view model's depth band so no wall comes
/// between.
pub(crate) fn place_inventory_puppet(
    ui: Res<frame::MinecraftUi>,
    mut puppet: ResMut<frame::InventoryPuppet>,
    mut view: ResMut<MinecraftWorldView>,
    local: Res<net::LocalPresentClient>,
    presented: Res<net::PresentedSnapshot>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cameras: Query<&Transform, With<render_scene::FlyCamera>>,
    lenses: Query<&Projection, With<render_scene::FpvLens>>,
) {
    use std::sync::atomic::Ordering::Relaxed;
    puppet.active = false;
    view.backdrop = None;
    render_frame::DEPTH_HACK_SCENE_ENTNUM.store(u32::MAX, Relaxed);
    let alive = presented.player(local.0).is_some_and(|ps| ps.pm_type == 0);
    if !(ui.active && ui.inventory_open && alive) {
        return;
    }
    let (Some([cx, cy, box_w, box_h]), Ok(window), Ok(camera), Ok(Projection::Perspective(lens))) =
        (ui.character_box, windows.single(), cameras.single(), lenses.single())
    else {
        return;
    };
    let (w, h) = (window.width().max(1.0), window.height().max(1.0));
    let tan_v = (lens.fov * 0.5).tan();
    let tan_h = tan_v * if lens.aspect_ratio > 1e-3 { lens.aspect_ratio } else { w / h };
    let (eye, fwd, right, up) = (camera.translation, *camera.forward(), *camera.right(), *camera.up());
    // A point `distance` along the view at a window pixel.
    let at = |px: f32, py: f32, distance: f32| {
        let (nx, ny) = (px / w * 2.0 - 1.0, 1.0 - py / h * 2.0);
        eye + (fwd + right * (nx * tan_h) + up * (ny * tan_v)) * distance
    };
    let distance = 10.0;
    let world_h = box_h / h * 2.0 * tan_v * distance;
    // A standing MW2 player is about 72 units: most of the window.
    let scale = world_h * 0.82 / 72.0;
    let feet = at(cx, cy + box_h * 0.5, distance) + up * (world_h * 0.07);
    // Turned by the mouse as vanilla's
    // `InventoryScreen.renderEntityInInventoryFollowsMouse` turns its body.
    let turn = (ui.gaze[0] * 1.2).atan() * 0.7;
    let x_axis = (-fwd * turn.cos() + right * turn.sin()).normalize_or(-fwd);
    let y_axis = up.cross(x_axis);
    puppet.root = Mat4::from_cols(
        (x_axis * scale).extend(0.0),
        (y_axis * scale).extend(0.0),
        (up * scale).extend(0.0),
        feet.extend(1.0),
    );
    puppet.pitch = (ui.gaze[1] * 1.2).atan().to_degrees() * 0.6;
    puppet.client = local.0.0;
    puppet.active = true;
    render_frame::DEPTH_HACK_SCENE_ENTNUM.store(local.0.0, Relaxed);
    // The card: the window and a margin the panel covers, behind it.
    let corner = |dx: f32, dy: f32| {
        let p = at(cx + dx * box_w * 0.6, cy + dy * box_h * 0.6, distance * 1.4);
        let b = sim::voxel::to_block(view.origin, p.to_array());
        [b[0] as f32, b[1] as f32, b[2] as f32]
    };
    view.backdrop = Some([corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)]);
}

pub(crate) fn register(app: &mut App) {
    app.add_systems(
        Update,
        place_inventory_puppet
            .after(crate::sync_camera_from_presented)
            .after(frame::PresentedPublished)
            .in_set(frame::ClientSet::Present),
    );
    app.init_resource::<MinecraftWorldView>()
        .init_resource::<frame::MinecraftUi>()
        .init_resource::<frame::McKeyInput>()
        .init_resource::<frame::McTerrainSource>()
        .init_resource::<frame::McEditLog>()
        .add_message::<frame::McWorldCommand>()
        .add_message::<frame::McWorldReport>()
        .init_resource::<frame::InventoryPuppet>()
        .insert_non_send(Runtime::default())
        .add_systems(
            Update,
            update
                .after(frame::PresentedPublished)
                .in_set(frame::ClientSet::Present),
        );
}

/// An arena the host builds instead of the generated terrain.
enum Built {
    /// Superflat over this many chunks around the spawn.
    Flat(i32),
    /// The stand-in MW2 map's geometry, voxelized.
    Replica(Arc<sim::SimContent>),
    /// The generated world around its biggest village near the spawn.
    Village,
}

/// How far from the world spawn a village arena looks, in blocks.
const VILLAGE_RANGE: i32 = 2048;
/// Blocks of open ground kept between a village's bounds and the border.
const VILLAGE_MARGIN: i32 = 12;

fn load(seed: i64, world: Option<std::path::PathBuf>, view_distance: i32, built: Option<Built>) -> Result<Loaded, String> {
    let root = assets::minecraft_map::root().ok_or_else(assets::minecraft_setup::status)?;
    let paths = DataPaths::under(&root);
    let registries = Arc::new(Registries::load(&paths)?);
    let packs = PackStack::open(vec![root.join("resourcepacks/local/minecraft-26.3")])
        .map_err(|e| e.to_string())?;
    let stream = TerrainStream::for_dimension(
        registries.clone(),
        seed,
        view_distance,
        Dimension::Overworld,
        world.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    let build = minecraft_terrain::mesh::build(&HandcraftedScene::default(), &packs)
        .map_err(|e| e.to_string())?;
    let mut scene = HandcraftedScene::streamed(stream.states.clone());
    let mut stream = stream;
    // A built arena: the host makes its chunks and shows only those, with
    // the spawn standing on the new ground.
    let mut arena_half_width = None;
    if let Some(Built::Village) = &built {
        let started = std::time::Instant::now();
        match stream.biggest_village(VILLAGE_RANGE) {
            Some(village) => {
                stream.respawn_around(village.centre);
                let half = (village.half_extent + VILLAGE_MARGIN).clamp(40, 128);
                diag::info!(
                    World,
                    "Minecraft village: {} pieces at {:?}, border {half}, found in {:.1}s",
                    village.pieces,
                    village.centre,
                    started.elapsed().as_secs_f64()
                );
                arena_half_width = Some(f64::from(half) - 2.0);
            }
            None => diag::warn!(World, "Minecraft village: none within {VILLAGE_RANGE} blocks of the spawn; playing around the spawn"),
        }
    }
    if let Some(Built::Replica(content)) = &built {
        let (sx, sy, sz) = stream.player_spawn;
        let origin = [sx, sy, sz];
        let started = std::time::Instant::now();
        let voxels = crate::minecraft_replica::voxelize(content, origin);
        let radius = (voxels.half_width / 16.0).ceil() as i32 + 1;
        let spawn_chunk = ((sx.floor() as i32) >> 4, (sz.floor() as i32) >> 4);
        stream.set_held_area(Some((spawn_chunk, radius)));
        for x in -radius..=radius {
            for z in -radius..=radius {
                let base = stream.generated_base((spawn_chunk.0 + x, spawn_chunk.1 + z));
                let chunk = crate::minecraft_replica::build_chunk(&stream, &base, &voxels);
                stream.provide_chunk(Arc::new(chunk), &mut scene);
            }
        }
        diag::info!(
            World,
            "Minecraft replica: {} blocks, half width {:.0}, floor {}, {} chunks in {:.1}s",
            voxels.blocks.len(),
            voxels.half_width,
            voxels.floor,
            (2 * radius + 1).pow(2),
            started.elapsed().as_secs_f64()
        );
        arena_half_width = Some(voxels.half_width);
    }
    if let Some(Built::Flat(radius)) = built {
        let (sx, sy, sz) = stream.player_spawn;
        let ground = sy.floor() as i32 - 1;
        let spawn_chunk = ((sx.floor() as i32) >> 4, (sz.floor() as i32) >> 4);
        stream.set_held_area(Some((spawn_chunk, radius)));
        for x in -radius..=radius {
            for z in -radius..=radius {
                let base = stream.generated_base((spawn_chunk.0 + x, spawn_chunk.1 + z));
                let flat = stream.flat_chunk(&base, ground);
                stream.provide_chunk(Arc::new(flat), &mut scene);
            }
        }
        stream.player_spawn = (sx, f64::from(ground + 1), sz);
    }
    let environment =
        DimensionEnvironment::load(&registries, Dimension::Overworld.dimension_type())?;
    let celestial = Arc::new(celestial_image(&packs).map_err(|e| e.to_string())?);
    let cloud_mask = CloudMask::from_pack(&packs).ok();
    let crack_texture = Arc::new(crate::minecraft_mining::crack_strip(&packs).map_err(|e| e.to_string())?);
    Ok(Loaded {
        stream,
        scene,
        packs,
        atlas: build.atlas,
        registries,
        seed,
        environment,
        celestial,
        cloud_mask,
        crack_texture,
        arena_half_width,
    })
}

/// The sky's sun and moon phases, laid out as MinecraftOSS lays them out.
fn celestial_image(packs: &PackStack) -> anyhow::Result<image::RgbaImage> {
    let mut celestial = image::RgbaImage::new(32 * 9, 32);
    let names = [
        "environment/celestial/sun",
        "environment/celestial/moon/full_moon",
        "environment/celestial/moon/waning_gibbous",
        "environment/celestial/moon/third_quarter",
        "environment/celestial/moon/waning_crescent",
        "environment/celestial/moon/new_moon",
        "environment/celestial/moon/waxing_crescent",
        "environment/celestial/moon/first_quarter",
        "environment/celestial/moon/waxing_gibbous",
    ];
    for (index, path) in names.iter().enumerate() {
        let id = minecraft_terrain::pack::ResourceId::parse(&format!("minecraft:{path}"))?;
        if let Some(bytes) = packs.texture(&id)? {
            let img =
                image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)?.to_rgba8();
            let tile =
                image::imageops::resize(&img, 32, 32, image::imageops::FilterType::Nearest);
            image::imageops::replace(&mut celestial, &tile, (index as i64) * 32, 0);
        }
    }
    Ok(celestial)
}

/// Vanilla's heart sprites side by side for the HUD's health bar: container,
/// full, half, and the container's hurt flash.
fn hearts_image(packs: &PackStack) -> anyhow::Result<image::RgbaImage> {
    const NAMES: [&str; 4] = ["container", "full", "half", "container_blinking"];
    let mut hearts = image::RgbaImage::new(9 * NAMES.len() as u32, 9);
    for (index, name) in NAMES.iter().enumerate() {
        let id = minecraft_terrain::pack::ResourceId::parse(&format!("minecraft:gui/sprites/hud/heart/{name}"))?;
        let bytes = packs.texture(&id)?.ok_or_else(|| anyhow::anyhow!("no heart sprite {name}"))?;
        let sprite = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)?.to_rgba8();
        let sprite = image::imageops::resize(&sprite, 9, 9, image::imageops::FilterType::Nearest);
        image::imageops::replace(&mut hearts, &sprite, index as i64 * 9, 0);
    }
    Ok(hearts)
}

/// A controller trigger, held or just pressed: with a block or an empty hand
/// the right trigger mines and the left places, as the mouse buttons do.
fn pad_trigger(pad: Option<&bevy::input::gamepad::Gamepad>, button: GamepadButton, just: bool) -> bool {
    pad.is_some_and(|pad| if just { pad.just_pressed(button) } else { pad.pressed(button) })
}

fn seed() -> i64 {
    if let Some(seed) = std::env::var("IW4L_MINECRAFT_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return seed;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (nanos as i64) ^ 0x5DEE_CE66_D1CE_4E5B
}

#[allow(clippy::too_many_arguments)]
fn update(
    time: Res<Time>,
    mut installed: MessageReader<frame::MatchInstalled>,
    mut torn_down: MessageReader<frame::MatchTornDown>,
    local: Res<net::LocalPresentClient>,
    presented: Res<net::PresentedSnapshot>,
    authority: Option<ResMut<net::AuthorityWorld>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    (mut ui, mut puppet, mut images, mut sound_queue, buttons): (
        ResMut<frame::MinecraftUi>,
        ResMut<frame::InventoryPuppet>,
        ResMut<Assets<Image>>,
        ResMut<audio::McSoundQueue>,
        Res<ButtonInput<MouseButton>>,
    ),
    mut view: ResMut<MinecraftWorldView>,
    mut runtime: NonSendMut<Runtime>,
    (skate, cameras, gamepads, active_pad, rules, prediction, master): (
        Res<frame::SkateMode>,
        Query<&Transform, With<render_scene::FlyCamera>>,
        Query<&bevy::input::gamepad::Gamepad>,
        Option<Res<frame::ActivePad>>,
        Option<Res<frame::HostMatchRules>>,
        Option<Res<net::ClientPredictionState>>,
        Option<Res<net::MasterBridge>>,
    ),
    (mut actions, mut action_ids, mut grants, mut reliable): (
        ResMut<net::ClientActionInbox>,
        ResMut<net::ActionRequestIds>,
        MessageReader<frame::McGrant>,
        ResMut<net::ReliableEventHub>,
    ),
    (mut world_commands, mut reports, mut exec, mut chunk_parts, mut terrain, mut edit_log, mut host_edits): (
        MessageReader<frame::McWorldCommand>,
        MessageWriter<frame::McWorldReport>,
        MessageWriter<frame::UiExecCommand>,
        MessageReader<frame::McChunkPart>,
        ResMut<frame::McTerrainSource>,
        ResMut<frame::McEditLog>,
        MessageReader<frame::McEditsReceived>,
    ),
) {
    for command in world_commands.read() {
        world_command(&mut runtime, command, &mut reports, &mut exec);
    }
    let pad = active_pad.and_then(|active| active.0).and_then(|entity| gamepads.get(entity).ok());
    for _ in torn_down.read() {
        stop(&mut runtime, &mut view);
        terrain.order.clear();
        terrain.chunks.clear();
    }
    // Until the spawn's ground is in collision (on a client, until the host's
    // settings have arrived and the world is built from them).
    ui.loading_world = view.active && !runtime.ready;
    for match_ in installed.read() {
        stop(&mut runtime, &mut view);
        terrain.order.clear();
        terrain.chunks.clear();
        if assets::minecraft_map::is_minecraft(&match_.zone) {
            view.active = true;
            if authority.is_none() {
                // A client builds the world the host describes.
                runtime.awaiting_settings = true;
                diag::info!(World, "Minecraft world: waiting for the host's world settings");
                continue;
            }
            let loading_save = runtime.pending_load.take();
            let seed = loading_save.as_ref().map_or_else(seed, |(meta, _)| meta.seed);
            let host_rule = |wanted: &str| {
                rules.as_ref().and_then(|rules| {
                    rules
                        .0
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
                        .map(|(_, value)| value.clone())
                })
            };
            let rule = host_rule("scr_mc_difficulty");
            // The border's half width in blocks (0: none), and the mobs:
            // auto is off in a lobby hosted for others, on alone.
            // An environment override (testing) wins over Game Setup.
            let border = std::env::var("IW4L_MINECRAFT_BORDER")
                .ok()
                .or_else(|| host_rule("scr_mc_border"))
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(0.0)
                .max(0.0);
            let hosting = master
                .as_ref()
                .is_some_and(|m| matches!(m.state(), net::MasterBridgeState::Hosting { .. }));
            let mobs = match std::env::var("IW4L_MINECRAFT_MOBS")
                .ok()
                .or_else(|| host_rule("scr_mc_mobs"))
                .as_deref()
                .map(str::trim)
            {
                Some("1" | "on") => true,
                Some("0" | "off") => false,
                _ => !hosting,
            };
            // A loaded save keeps its own difficulty.
            let saved = loading_save.as_ref().map(|(meta, _)| meta.difficulty.clone());
            let difficulty = crate::minecraft_entities::parse_difficulty(
                saved
                    .or(rule)
                    .or_else(|| std::env::var("IW4L_MINECRAFT_DIFFICULTY").ok())
                    .as_deref(),
            );
            runtime.difficulty = Some(difficulty);
            let kit = match std::env::var("IW4L_MINECRAFT_BLOCKS")
                .ok()
                .or_else(|| host_rule("scr_mc_blocks"))
                .as_deref()
                .map(str::trim)
            {
                Some("kit") => true,
                Some("survival") => false,
                _ => hosting,
            };
            // A replica map is its replica; the generated world takes Game
            // Setup's MINECRAFT WORLD.
            let kind = match assets::minecraft_map::world_kind(&match_.zone) {
                Some(kind) => WorldKind::parse(kind),
                None => WorldKind::parse(
                    &std::env::var("IW4L_MINECRAFT_WORLD").ok().or_else(|| host_rule("scr_mc_world")).unwrap_or_default(),
                ),
            };
            // Mobs live in the generated terrain, which a built arena replaces.
            let mobs = mobs && matches!(kind, WorldKind::Natural | WorldKind::Village);
            let content = authority.as_ref().map(|authority| authority.0.content());
            let built = match (kind, content) {
                (WorldKind::Flat, _) => Some(Built::Flat(arena_radius(border))),
                (WorldKind::Replica, Some(content)) => Some(Built::Replica(content)),
                // Only the host searches: a client stands where it's told.
                (WorldKind::Village, Some(_)) => Some(Built::Village),
                _ => None,
            };
            // A replica's own spawn points and size decide the arena.
            let view_distance_floor = match kind {
                WorldKind::Replica | WorldKind::Village => 12,
                _ => 0,
            };
            runtime.settings = Some(WorldSettings { seed, origin: None, border, mobs, kit, kind, difficulty, check: None });
            diag::info!(World, "Minecraft world: seed {seed}, difficulty {difficulty:?}, border {border}, mobs {mobs}");
            let world_dir = loading_save.as_ref().map(|(_, dir)| dir.clone());
            // The host keeps the whole arena loaded around its centre.
            let view_distance = VIEW_DISTANCE.max(arena_radius(border) + 1).max(view_distance_floor);
            runtime.restore = loading_save.map(|(meta, _)| meta);
            let (send, receive) = mpsc::channel();
            let _ = std::thread::Builder::new()
                .name("minecraft-world-load".into())
                .spawn(move || {
                    let _ = send.send(load(seed, world_dir, view_distance, built));
                });
            runtime.loading = Some(receive);
        }
    }
    let mut authority = authority;

    // A client loads once the host's settings arrive with a snapshot.
    if runtime.awaiting_settings
        && let Some(settings) =
            presented.snapshot().and_then(|s| WorldSettings::from_server_info(&s.meta.objectives.server_info))
    {
        runtime.awaiting_settings = false;
        runtime.difficulty = Some(settings.difficulty);
        runtime.settings = Some(settings);
        diag::info!(World, "Minecraft world: host's settings {settings:?}");
        let (send, receive) = mpsc::channel();
        let seed = settings.seed;
        let _ = std::thread::Builder::new()
            .name("minecraft-world-load".into())
            .spawn(move || {
                let _ = send.send(load(seed, None, VIEW_DISTANCE, None));
            });
        runtime.loading = Some(receive);
    }

    // A client's arena terrain from its host, whole chunks once all their
    // pieces are in.
    for part in chunk_parts.read() {
        let (generation, pieces) = &mut runtime.chunk_parts;
        if *generation != part.generation {
            *generation = part.generation;
            pieces.clear();
        }
        let slots = pieces.entry(part.pos).or_insert_with(|| vec![None; usize::from(part.parts)]);
        if let Some(slot) = slots.get_mut(usize::from(part.part)) {
            *slot = Some(part.data.clone());
        }
        if slots.iter().all(Option::is_some) {
            let bytes: Vec<u8> = slots.iter().flatten().flat_map(|piece| piece.iter().copied()).collect();
            pieces.remove(&part.pos);
            runtime.chunks_waiting.push(bytes);
        }
    }
    for received in host_edits.read() {
        if runtime.next_edit.0 != received.generation {
            runtime.next_edit = (received.generation, 1);
            runtime.edits_waiting.clear();
        }
        runtime.edits_waiting.push((received.first_seq, received.edits.clone()));
    }
    let held = &mut *runtime;
    if let Some(world) = held.world.as_mut() {
        for bytes in std::mem::take(&mut held.chunks_waiting) {
            match world.stream.decode_chunk(&bytes) {
                Ok(chunk) => world.stream.provide_chunk(std::sync::Arc::new(chunk), &mut world.scene),
                Err(error) => diag::warn!(World, "Minecraft: a host chunk failed to decode: {error:#}"),
            }
        }
    }

    if let Some(receive) = &runtime.loading
        && let Ok(result) = receive.try_recv()
    {
        runtime.loading = None;
        match result {
            Ok(mut world) => {
                // A loaded save starts the player where they were saved: the
                // spawn below waits for that ground like any other.
                if let Some(meta) = &runtime.restore {
                    world.stream.player_spawn = (meta.feet[0], meta.feet[1], meta.feet[2]);
                }
                // A client stands its world where the host's does.
                if let Some([x, y, z]) = runtime.settings.and_then(|s| s.origin) {
                    world.stream.player_spawn = (x, y, z);
                }
                let (x, y, z) = world.stream.player_spawn;
                view.origin = [x, y, z];
                if let Some(settings) = runtime.settings.as_mut() {
                    settings.origin = Some(view.origin);
                    if let Some(half) = world.arena_half_width {
                        settings.border = half.ceil() + 2.0;
                    }
                }
                view.atlas = Some(world.atlas.clone());
                view.celestial = Some(world.celestial.clone());
                view.crack_texture = Some(world.crack_texture.clone());
                runtime.mining = Default::default();
                runtime.sounds = Some(crate::minecraft_sounds::Sounds::load(&world.packs));
                if runtime.hearts.is_none() {
                    match hearts_image(&world.packs) {
                        Ok(hearts) => {
                            runtime.hearts = Some(images.add(Image {
                                sampler: bevy::image::ImageSampler::nearest(),
                                ..Image::new(
                                    bevy::render::render_resource::Extent3d {
                                        width: hearts.width(),
                                        height: hearts.height(),
                                        depth_or_array_layers: 1,
                                    },
                                    bevy::render::render_resource::TextureDimension::D2,
                                    hearts.into_raw(),
                                    bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                                    bevy::asset::RenderAssetUsages::default(),
                                )
                            }));
                        }
                        Err(error) => diag::warn!(World, "Minecraft hearts unavailable: {error:#}"),
                    }
                }
                runtime.entities = Some(crate::minecraft_entities::Entities::new(
                    &world.stream,
                    world.seed,
                    runtime.difficulty.unwrap_or(minecraftoss_player::Difficulty::Normal),
                    runtime.settings.is_none_or(|s| s.mobs),
                ));
                runtime.day = DayCycle::default();
                // Game ticks since sunrise to start at: 6000 noon, 13000
                // dusk, 18000 midnight.
                if let Some(ticks) = std::env::var("IW4L_MINECRAFT_TIME")
                    .ok()
                    .and_then(|t| t.trim().parse::<f64>().ok())
                {
                    runtime.day.set(ticks);
                }
                if let Some(meta) = runtime.restore.take() {
                    runtime.day.set(meta.day_ticks);
                    if let Some(entities) = runtime.entities.as_mut() {
                        let slots = &mut entities.inventory.slots;
                        for (slot, saved) in slots.iter_mut().zip(&meta.slots) {
                            *slot = saved.as_ref().map(crate::minecraft_saves::SavedStack::stack);
                        }
                        entities.selected = meta.selected.min(frame::minecraft_ui::MC_HOTBAR - 1);
                    }
                }
                runtime.environment_accumulator = 0.0;
                runtime.environment_primed = false;
                runtime.light = Some(SkyLight::streamed());
                runtime.light_volume_at = None;
                runtime.cloud_center = None;
                view.generation += 1;
                // Collision against the blocks for whoever traces this
                // match's brushes here: the authority on the host (whose
                // prediction shares its content), prediction on a client.
                let content = match (authority.as_ref(), prediction.as_ref()) {
                    (Some(authority), _) => Some(authority.0.content()),
                    (None, Some(prediction)) => Some(prediction.0.world().content()),
                    (None, None) => None,
                };
                if let Some(content) = content {
                    sim::voxel::activate(content.clip_brushes(), view.origin, vec![Vec::new()]);
                }
                sim::voxel::set_border(
                    runtime
                        .settings
                        .filter(|s| s.border > 0.0)
                        .map(|s| ([view.origin[0].floor() + 0.5, view.origin[2].floor() + 0.5], s.border)),
                );
                // The arena's chunks: a client takes them from its host; the
                // host encodes them for its clients, nearest the spawn first.
                let spawn_chunk = ((view.origin[0].floor() as i32) >> 4, (view.origin[2].floor() as i32) >> 4);
                let radius = arena_radius(runtime.settings.map_or(0.0, |s| s.border));
                if authority.is_none() {
                    world.stream.set_held_area(Some((spawn_chunk, radius)));
                } else {
                    let mut order: Vec<[i32; 2]> = (-radius..=radius)
                        .flat_map(|x| (-radius..=radius).map(move |z| [spawn_chunk.0 + x, spawn_chunk.1 + z]))
                        .collect();
                    order.sort_by_key(|[x, z]| ((x - spawn_chunk.0).pow(2) + (z - spawn_chunk.1).pow(2), *x, *z));
                    terrain.generation = terrain.generation.wrapping_add(1);
                    terrain.order = order;
                    terrain.chunks.clear();
                    edit_log.generation = terrain.generation;
                    edit_log.edits.clear();
                    runtime.terrain_next = 0;
                }
                runtime.ready = false;
                runtime.spawn_check = None;
                diag::info!(
                    World,
                    "Minecraft world ready: seed {} spawn {:?}",
                    world.seed,
                    world.stream.player_spawn
                );
                runtime.world = Some(world);
                runtime.shapes.clear();
                runtime.shape_ids.clear();
                runtime.spawned.clear();
            }
            Err(error) => {
                diag::warn!(World, "Minecraft world failed to load: {error}");
                view.active = false;
            }
        }
    }

    let origin = view.origin;
    let Runtime {
        world,
        shapes,
        shape_ids,
        spawned,
        spawn_counts,
        day,
        environment_accumulator,
        environment_primed,
        light,
        light_volume_at,
        light_volume_age,
        cloud_center,
        mining,
        entities,
        inventory_ui,
        sounds,
        hand,
        minimap,
        steps,
        last_feet,
        hearts,
        settings,
        ready,
        spawn_check,
        terrain_next,
        edits_waiting,
        next_edit,
        kit_given,
        scripted_use,
        ..
    } = &mut *runtime;
    let Some(world) = world.as_mut() else {
        ui.active = false;
        ui.inventory_open = false;
        puppet.active = false;
        return;
    };
    let ps = presented.player(local.0);

    // Every spawn lands on the Minecraft spawn once its ground exists.
    let spawn_chunk = ((origin[0].floor() as i32) >> 4, (origin[2].floor() as i32) >> 4);
    let alive = ps.is_some_and(|ps| ps.pm_type == 0);
    // The host places every player (remote ones too: the stand-in map's own
    // spawn points can lie outside the border) on the spawn once its ground
    // exists, retried each frame until the authority has them to move.
    if let Some(authority) = authority.as_mut()
        && let Some(snapshot) = presented.snapshot()
    {
        let ground = world.scene.generated_chunk(spawn_chunk).is_some();
        let border = settings.map_or(0.0, |s| s.border);
        for (id, state) in &snapshot.players {
            if state.pm_type != 0 {
                spawned.remove(id);
            } else if settings.is_some_and(|s| s.kind == WorldKind::Replica) {
                // A replica keeps the map's own spawn points.
                spawned.insert(*id);
            } else if ground && !spawned.contains(id) {
                // Teams on opposite sides of the arena, everyone else spread
                // around it, on ground checked at every spawn.
                let team = snapshot.meta.for_client(*id).map_or(0, |m| m.client_state_team);
                let count = spawn_counts.entry(*id).or_default();
                let point = spawn_point(world, origin, border, team, u64::from(id.0) * 7919 + u64::from(*count));
                let target = point.map_or([0.0, 0.0, 0.0], |p| sim::voxel::to_map(origin, p));
                if authority.0.teleport(*id, target) {
                    diag::info!(World, "Minecraft spawn: client {} (team {team}) at {point:?}", id.0);
                    spawned.insert(*id);
                    *count += 1;
                }
            }
        }
        spawned.retain(|id| snapshot.players.iter().any(|(player, _)| player == id));
    }

    let feet = ps.map(|ps| sim::voxel::to_block(origin, ps.origin));
    if feet.is_some() {
        *last_feet = feet;
    }
    // A host with a border keeps the whole arena loaded around its centre,
    // so every player inside it has ground; otherwise the world streams
    // around the local player, or the spawn until there is one.
    let arena = authority.is_some() && settings.is_some_and(|s| s.border > 0.0);
    let centre = feet.filter(|_| !arena).unwrap_or(origin);
    let block = (
        centre[0].floor() as i32,
        centre[1].floor() as i32,
        centre[2].floor() as i32,
    );
    let (loaded, forgotten) = world.stream.server_tick(block, &mut world.scene);
    for chunk in loaded {
        let blocks = &world.registries.blocks;
        let (min_y, height) = (chunk.min_y(), chunk.height());
        let mut ids = vec![0u16; (height * 256) as usize];
        // Runs of one state (air, stone) skip the map.
        let mut last = None;
        for y in 0..height {
            for z in 0..16usize {
                for x in 0..16usize {
                    let state = chunk.block(x, min_y + y, z);
                    if let Some((last_state, id)) = last
                        && last_state == state
                    {
                        ids[((y as usize * 16) + z) * 16 + x] = id;
                        continue;
                    }
                    let id = *shapes.entry(state).or_insert_with(|| {
                        let boxes = blocks.collision_boxes(state);
                        if boxes.is_empty() {
                            return 0;
                        }
                        let key: Vec<[u32; 6]> =
                            boxes.iter().map(|b| b.map(|v| (v as f32).to_bits())).collect();
                        if let Some(&id) = shape_ids.get(&key) {
                            return id;
                        }
                        let boxes32 = boxes.iter().map(|b| b.map(|v| v as f32)).collect();
                        let id = sim::voxel::add_shapes(vec![boxes32]).unwrap_or(0);
                        shape_ids.insert(key, id);
                        id
                    });
                    last = Some((state, id));
                    ids[((y as usize * 16) + z) * 16 + x] = id;
                }
            }
        }
        // Edits stand over the chunk as generated (a client's host chunks are
        // the generated ones; edits arrive on their own, maybe earlier).
        let (placed, cleared) = world.scene.chunk_edits((chunk.pos.x, chunk.pos.z));
        let index = |(x, y, z): (i32, i32, i32)| {
            let ly = y - min_y;
            (0..height).contains(&ly).then(|| ((ly as usize * 16 + (z & 15) as usize) * 16) + (x & 15) as usize)
        };
        for (pos, block) in placed.iter().flat_map(|placed| placed.iter()) {
            if let Some(i) = index(*pos) {
                let state = world.stream.states.state_of(block);
                ids[i] = shape_for_state(state, &world.registries.blocks, shapes, shape_ids);
            }
        }
        for pos in cleared.iter().flat_map(|cleared| cleared.iter()) {
            if let Some(i) = index(*pos) {
                ids[i] = 0;
            }
        }
        if let Some(entities) = entities.as_mut() {
            entities.load_chunk(&chunk);
        }
        sim::voxel::set_chunk(
            chunk.pos.x,
            chunk.pos.z,
            sim::voxel::VoxelChunk {
                min_y,
                height,
                shapes: ids,
            },
        );
    }
    for pos in forgotten {
        sim::voxel::remove_chunk(pos.x, pos.z);
        if let Some(entities) = entities.as_mut() {
            entities.unload_chunk(pos);
        }
    }

    // The base world's identity: the spawn chunk's blocks, which the host
    // publishes and a client checks against its own.
    if spawn_check.is_none()
        && let Some(chunk) = world.scene.generated_chunk(spawn_chunk)
    {
        let check = chunk_checksum(chunk);
        *spawn_check = Some(check);
        match settings.as_mut() {
            Some(settings) if authority.is_some() => settings.check = Some(check),
            Some(settings) => match settings.check {
                Some(host) if host != check => {
                    diag::error!(
                        World,
                        "Minecraft world MISMATCH: the host's spawn chunk is {host:016x}, this machine generated {check:016x}; the terrain will differ"
                    );
                    reports.write(frame::McWorldReport(
                        "WARNING: this machine's Minecraft terrain differs from the host's".to_owned(),
                    ));
                }
                Some(_) => diag::info!(World, "Minecraft world: spawn chunk matches the host ({check:016x})"),
                None => {}
            },
            None => {}
        }
    }
    // Ready once the spawn's ground and its neighbours are in (a client
    // waits for its host's chunks there).
    let was_ready = *ready;
    *ready = spawn_check.is_some()
        && (-1..=1).all(|x| (-1..=1).all(|z| world.scene.generated_chunk((spawn_chunk.0 + x, spawn_chunk.1 + z)).is_some()));
    if *ready && !was_ready {
        diag::info!(World, "Minecraft world: spawn area in, {} chunks loaded", world.scene.generated_chunks().count());
    }
    // The host encodes the arena for clients a few chunks a frame.
    if authority.is_some() {
        let started = std::time::Instant::now();
        while let Some(&[x, z]) = terrain.order.get(*terrain_next) {
            let Some(chunk) = world.scene.generated_chunk((x, z)) else {
                break;
            };
            let bytes = world.stream.encode_chunk(chunk);
            terrain.chunks.insert([x, z], bytes.into());
            *terrain_next += 1;
            if started.elapsed().as_secs_f64() > 0.004 {
                break;
            }
        }
    }

    // The host tells clients which world to build (`mc_*` server info).
    if let (Some(authority), Some(settings)) = (authority.as_mut(), settings.as_ref()) {
        settings.publish(&mut authority.0, day.ticks);
    } else if let Some(host_ticks) = presented
        .snapshot()
        .and_then(|s| s.meta.objectives.server_info("mc_time"))
        .and_then(|t| t.parse::<f64>().ok())
        && (host_ticks - day.ticks).abs() > 100.0
    {
        // A client's sky follows the host's clock.
        day.set(host_ticks);
    }

    let Some(ps) = ps else {
        ui.active = false;
        puppet.active = false;
        return;
    };
    let feet = feet.unwrap_or(origin);
    // Minecraft's yaw: 0 facing +Z (map -Y), turning towards -X.
    let yaw_rad = ps.viewangles[1].to_radians();
    let mc_yaw = (-yaw_rad.cos()).atan2(-yaw_rad.sin()).to_degrees();
    let mut all_events = sim::voxel::take_events();
    // Blocks the host changed this frame, logged for its clients.
    let mut changed: Vec<(i32, i32, i32)> = Vec::new();
    // A client applies its host's edits, in order.
    if authority.is_none() {
        let mut waiting = std::mem::take(edits_waiting);
        waiting.sort_by_key(|(first, _)| *first);
        for (first, edits) in waiting {
            let live = first != 0;
            if live && first != next_edit.1 {
                diag::warn!(World, "Minecraft: host edit {first} arrived, {} expected", next_edit.1);
            }
            if live {
                next_edit.1 = first + edits.len() as u32;
            }
            let mut positions = Vec::with_capacity(edits.len());
            for ([x, y, z], state) in edits {
                let pos = (x, y, z);
                let old = minecraft_terrain::scene::Scene::block(&world.scene, pos).cloned();
                let block = world.stream.states.block(minecraftoss_core::BlockStateId(state)).cloned();
                if live
                    && block.is_none()
                    && let (Some(sounds), Some(kind)) = (sounds.as_mut(), old.as_ref().and_then(|b| world.scene.sound_type(b)))
                {
                    let centre = [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5];
                    let at = Vec3::from_array(sim::voxel::to_map(origin, centre));
                    sounds.play(&world.packs, &kind.break_sound, Some(at), (kind.volume + 1.0) / 2.0, kind.pitch * 0.8);
                }
                let shape = shape_for_state(
                    block.as_ref().and_then(|b| world.stream.states.state_of(b)),
                    &world.registries.blocks,
                    shapes,
                    shape_ids,
                );
                world.scene.set_authoritative(pos, block);
                sim::voxel::set_block_shape(x, y, z, shape);
                positions.push(pos);
            }
            world.stream.record_edits(&world.scene, &positions);
            world.stream.mark_edited(&world.scene, &positions);
            diag::info!(
                World,
                "Minecraft: applied {} host edits ({})",
                positions.len(),
                if live { format!("from {first}") } else { "join state".to_owned() }
            );
        }
    }

    // The hand, when no gun is selected: vanilla's left click mines by hand
    // (with the hand's break speed) or punches, its right click places the
    // held block (`Player.place_selected`, vanilla's placement states).
    // Footsteps (`Entity.applyMovementEmissionAndPlaySound`): the walked
    // distance grows by 0.6 of each horizontal move on the ground, and past
    // the next step the block under the feet sounds its step at 0.15 of its
    // volume. A fall of more than three blocks lands with the block's fall
    // sound and the player's (`LivingEntity.causeFallDamage`).
    let on_ground = alive && ps.ground_entity_num != playerstate_iw4::ENTITYNUM_NONE;
    if let Some(last) = steps.last.filter(|_| alive) {
        let horizontal = ((feet[0] - last[0]).hypot(feet[2] - last[2]) * 0.6) as f32;
        let block_at = |dy: f64| {
            let pos = (feet[0].floor() as i32, (feet[1] - dy).floor() as i32, feet[2].floor() as i32);
            minecraft_terrain::scene::Scene::block(&world.scene, pos).cloned().map(|b| (pos, b))
        };
        let under = block_at(0.2);
        let centre = |pos: (i32, i32, i32)| {
            Vec3::from_array(sim::voxel::to_map(origin, [pos.0 as f64 + 0.5, pos.1 as f64 + 1.0, pos.2 as f64 + 0.5]))
        };
        if on_ground && horizontal < 2.0 {
            steps.move_dist += horizontal;
            if steps.move_dist > steps.next_step
                && let Some((pos, block)) = under.as_ref()
            {
                steps.next_step = steps.move_dist as i32 as f32 + 1.0;
                // Snow layers and carpets sound instead of what they lie on.
                let inside = block_at(-0.01).filter(|(_, b)| {
                    let p = b.id.path.as_str();
                    p == "snow" || p.ends_with("_carpet") || p == "moss_carpet"
                });
                let (pos, block) = inside.as_ref().map_or((*pos, block), |(p, b)| (*p, b));
                if let (Some(sounds), Some(kind)) = (sounds.as_mut(), world.scene.sound_type(block)) {
                    sounds.play(&world.packs, &kind.step, Some(centre(pos)), kind.volume * 0.15, kind.pitch);
                }
            }
        }
        if on_ground {
            if let Some(peak) = steps.air_peak.take() {
                let fall = peak - feet[1];
                if fall > 3.0
                    && let Some(sounds) = sounds.as_mut()
                {
                    let event = if fall > 7.0 { "minecraft:entity.player.big_fall" } else { "minecraft:entity.player.small_fall" };
                    sounds.play(&world.packs, event, None, 1.0, 1.0);
                    if let Some((pos, block)) = under.as_ref()
                        && let Some(kind) = world.scene.sound_type(block)
                    {
                        sounds.play(&world.packs, &kind.fall, Some(centre(*pos)), kind.volume * 0.5, kind.pitch * 0.75);
                    }
                }
            }
        } else {
            steps.air_peak = Some(steps.air_peak.map_or(feet[1], |p| p.max(feet[1])));
        }
    } else {
        steps.air_peak = None;
    }
    steps.last = alive.then_some(feet);

    let dt_hand = time.delta_secs_f64();
    if let Some(ticks) = hand.swing.as_mut() {
        *ticks += (dt_hand * 20.0) as f32;
        if *ticks >= crate::minecraft_hand::SWING_TICKS {
            hand.swing = None;
        }
    }
    let holding = entities.as_ref().is_some_and(|e| {
        e.inventory.slots[e.selected].as_ref().is_none_or(|s| crate::minecraft_inventory::weapon_of(s).is_none())
    });
    ui.holding_item = alive && holding;
    ui.empty_hand = ui.holding_item
        && entities.as_ref().is_some_and(|e| e.inventory.slots[e.selected].is_none());
    hand.clock += dt_hand;
    let hand_ticks = (hand.clock / TICK_SECONDS) as u32;
    hand.clock -= f64::from(hand_ticks) * TICK_SECONDS;
    // Mining by hand stays the host's; a client places by asking the host
    // (`ClientAction::McPlace`), its world changing when the edit comes back.
    let host = authority.is_some();
    let players: Vec<[f64; 3]> = presented
        .snapshot()
        .map(|s| {
            s.players
                .iter()
                .filter(|(_, p)| p.pm_type == 0)
                .map(|(_, p)| sim::voxel::to_block(origin, p.origin))
                .collect()
        })
        .unwrap_or_default();
    if let Some(entities) = entities.as_mut()
        && ui.holding_item
        && !ui.inventory_open
    {
        let mut player = minecraftoss_player::Player::new(glam::DVec3::from_array(feet));
        player.yaw = f64::from(mc_yaw);
        player.pitch = f64::from(ps.viewangles[0]);
        player.selected = entities.selected;
        let eye_block = glam::DVec3::from_array(feet) + glam::DVec3::Y * 1.62;
        let look = {
            let (yaw, pitch) = (f64::from(mc_yaw).to_radians(), f64::from(ps.viewangles[0]).to_radians());
            glam::DVec3::new(-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos())
        };
        if host && (buttons.just_pressed(MouseButton::Left) || pad_trigger(pad, GamepadButton::RightTrigger2, true)) {
            hand.swing = Some(0.0);
            entities.punch(eye_block, look, mc_yaw);
        }
        if host && (buttons.pressed(MouseButton::Left) || pad_trigger(pad, GamepadButton::RightTrigger2, false)) {
            for _ in 0..hand_ticks {
                if let Some(hit) = player.target(&world.scene, 4.5) {
                    // A hand mines as vanilla's `getDestroyProgress`: a
                    // block's hardness times thirty ticks.
                    all_events.push(sim::voxel::VoxelEvent::Shot {
                        block: [hit.pos.0, hit.pos.1, hit.pos.2],
                        damage: 160.0 / 30.0,
                    });
                    if hand.swing.is_none_or(|t| t >= crate::minecraft_hand::SWING_TICKS * 0.5) {
                        hand.swing = Some(0.0);
                    }
                }
            }
        }
        hand.place_delay = hand.place_delay.saturating_sub(hand_ticks);
        let place = (buttons.just_pressed(MouseButton::Right) || pad_trigger(pad, GamepadButton::LeftTrigger2, true))
            || ((buttons.pressed(MouseButton::Right) || pad_trigger(pad, GamepadButton::LeftTrigger2, false)) && hand.place_delay == 0)
            || std::mem::take(scripted_use);
        if place && !host {
            hand.place_delay = 4;
            // Where the block would go, worked out on a copy of the world.
            let mut preview = world.scene.clone();
            if let Some(pos) = player.place_selected(&mut preview, &mut entities.inventory, minecraftoss_player::GameMode::Survival)
                && let Some(block) = minecraft_terrain::scene::Scene::block(&preview, pos).cloned()
                && let Some(state) = world.stream.states.state_of(&block)
            {
                let request_id = action_ids.allocate();
                let _ = actions.push(
                    local.0,
                    sim::ClientAction::McPlace { request_id, pos: [pos.0, pos.1, pos.2], state: state.0 },
                );
                hand.swing = Some(0.0);
            }
        } else if place {
            hand.place_delay = 4;
            if let Some(pos) = player.place_selected(
                &mut world.scene,
                &mut entities.inventory,
                minecraftoss_player::GameMode::Survival,
            ) {
                changed.push(pos);
                // Not into anyone's box, nor past the border.
                let inside = block_hits_player(pos, &players) || block_hits_player(pos, &[feet])
                    || !sim::voxel::inside_border(pos.0, pos.2);
                let block = minecraft_terrain::scene::Scene::block(&world.scene, pos).cloned();
                if inside {
                    world.scene.set(pos, None);
                    if let Some(block) = block {
                        let _ = entities.inventory.add_item(
                            minecraftoss_player::inventory::ItemStack::new(block.id.key(), 1),
                            entities.selected,
                        );
                    }
                } else if let Some(block) = block {
                    let state = world.stream.states.state_of(&block);
                    let blocks = &world.registries.blocks;
                    let shape = state.map_or(0, |state| {
                        *shapes.entry(state).or_insert_with(|| {
                            let boxes = blocks.collision_boxes(state);
                            if boxes.is_empty() {
                                return 0;
                            }
                            let key: Vec<[u32; 6]> = boxes.iter().map(|b| b.map(|v| (v as f32).to_bits())).collect();
                            if let Some(&id) = shape_ids.get(&key) {
                                return id;
                            }
                            let boxes32 = boxes.iter().map(|b| b.map(|v| v as f32)).collect();
                            let id = sim::voxel::add_shapes(vec![boxes32]).unwrap_or(0);
                            shape_ids.insert(key, id);
                            id
                        })
                    });
                    sim::voxel::set_block_shape(pos.0, pos.1, pos.2, shape);
                    world.stream.record_edits(&world.scene, &[pos]);
                    world.stream.mark_edited(&world.scene, &[pos]);
                    entities.placed(&world.scene, pos);
                    hand.swing = Some(0.0);
                    if let (Some(sounds), Some(kind)) = (sounds.as_mut(), world.scene.sound_type(&block)) {
                        let centre = [pos.0 as f64 + 0.5, pos.1 as f64 + 0.5, pos.2 as f64 + 0.5];
                        let at = Vec3::from_array(sim::voxel::to_map(origin, centre));
                        sounds.play(&world.packs, &kind.place, Some(at), (kind.volume + 1.0) / 2.0, kind.pitch * 0.8);
                    }
                }
            }
        }
    }

    // Shots and explosions from the authoritative game: bullets that met a
    // mob hurt it, the rest mine.
    let (mob_shots, events): (Vec<_>, Vec<_>) = all_events
        .into_iter()
        .partition(|event| matches!(event, sim::voxel::VoxelEvent::MobShot { .. }));
    if let Some(entities) = entities.as_mut() {
        for shot in mob_shots {
            if let sim::voxel::VoxelEvent::MobShot { key, damage, from } = shot {
                entities.shoot(key, damage, from, mc_yaw);
            }
        }
    }
    // Vanilla's block sounds: a hit for each bullet into a block, then the
    // break of each block broken (`SoundType` volume and pitch as
    // `MultiPlayerGameMode` and `LevelRenderer` scale them), and blasts.
    let at = |b: [f64; 3]| Vec3::from_array(sim::voxel::to_map(origin, b));
    if let Some(sounds) = sounds.as_mut() {
        for event in &events {
            match *event {
                sim::voxel::VoxelEvent::Shot { block, .. } => {
                    let pos = (block[0], block[1], block[2]);
                    if let Some(kind) = minecraft_terrain::scene::Scene::block(&world.scene, pos)
                        .and_then(|b| world.scene.sound_type(b))
                    {
                        let centre = [block[0] as f64 + 0.5, block[1] as f64 + 0.5, block[2] as f64 + 0.5];
                        sounds.play(&world.packs, &kind.hit, Some(at(centre)), (kind.volume + 1.0) / 4.0, kind.pitch * 0.5);
                    }
                }
                sim::voxel::VoxelEvent::Explosion { center } => {
                    let pitch = (1.0 + (sounds.random() - sounds.random()) * 0.2) * 0.7;
                    sounds.play(&world.packs, "minecraft:entity.generic.explode", Some(at(center)), 4.0, pitch);
                }
                sim::voxel::VoxelEvent::MobShot { .. } | sim::voxel::VoxelEvent::Ray { .. } => {}
            }
        }
    }
    let broken = mining.apply(
        events,
        &mut crate::minecraft_mining::WorldRefs {
            stream: &mut world.stream,
            scene: &mut world.scene,
            packs: &world.packs,
            atlas: &world.atlas,
            registries: &world.registries,
        },
        time.elapsed_secs_f64(),
    );
    changed.extend(broken.iter().map(|(pos, ..)| *pos));
    if let Some(sounds) = sounds.as_mut() {
        for (pos, block, blast) in &broken {
            if *blast {
                continue;
            }
            if let Some(kind) = world.scene.sound_type(block) {
                let centre = [pos.0 as f64 + 0.5, pos.1 as f64 + 0.5, pos.2 as f64 + 0.5];
                sounds.play(&world.packs, &kind.break_sound, Some(at(centre)), (kind.volume + 1.0) / 2.0, kind.pitch * 0.8);
            }
        }
    }
    if let Some(entities) = entities.as_mut() {
        let positions: Vec<_> = broken.iter().map(|(pos, ..)| *pos).collect();
        entities.broke(&world.scene, &positions);
        entities.drop_blocks(&broken);
    }

    let eye = sim::voxel::to_block(origin, [ps.origin[0], ps.origin[1], ps.origin[2] + ps.view_height_current]);
    let (pitch, yaw) = (ps.viewangles[0].to_radians(), ps.viewangles[1].to_radians());
    let map_forward = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), -pitch.sin()];
    let forward = glam::Vec3::new(map_forward[0], map_forward[2], -map_forward[1]);
    let aspect = windows
        .single()
        .map(|w| w.width() / w.height().max(1.0))
        .unwrap_or(16.0 / 9.0);
    // Culled from the camera that draws: the player's eye, or while
    // skating the Skate camera (a frame behind, so with room to spare).
    let skate_camera = cameras.iter().next().filter(|_| skate.active).map(|t| {
        let at = sim::voxel::to_block(origin, t.translation.to_array());
        let ahead = t.rotation * Vec3::NEG_Z;
        (
            glam::DVec3::new(at[0], at[1], at[2]),
            glam::Vec3::new(ahead.x, ahead.z, -ahead.y).normalize_or(forward),
        )
    });
    let (cull_at, cull_forward) = skate_camera.unwrap_or((glam::DVec3::new(eye[0], eye[1], eye[2]), forward));
    let camera = CullCamera {
        position: cull_at,
        forward: cull_forward,
        fov_degrees: if skate_camera.is_some() { 120.0 } else { 90.0 },
        aspect,
        yaw_degrees: (-cull_forward.x).atan2(cull_forward.z).to_degrees(),
        pitch_degrees: (-cull_forward.y).asin().to_degrees(),
    };
    let update = world
        .stream
        .frame(&world.scene, &camera, FADE_MILLIS, &world.atlas, &world.packs);
    view.uploads.extend(update.uploads);
    view.removed.extend(update.removed);
    view.visible = update.visible;
    let Some(light) = light.as_mut() else {
        return;
    };
    for (chunk, column) in update.lights {
        light.set_chunk_column(chunk, column);
    }

    if let Some(sounds) = sounds.as_mut() {
        sound_queue.0.append(&mut sounds.queued);
    }
    // The minimap's picture, and its corners on the map.
    ui.minimap = minimap
        .update(time.delta_secs_f64(), feet, &world.scene, &world.packs, &world.atlas, &mut images)
        .map(|(image, [bx, bz])| {
            let corner = |x: i32, z: i32| {
                let p = sim::voxel::to_map(origin, [f64::from(x), feet[1], f64::from(z)]);
                [p[0], p[1]]
            };
            (image, corner(bx, bz), corner(bx + 256, bz + 256))
        });
    // The Overworld clock and the environment attributes of MinecraftOSS.
    let dt = time.delta_secs_f64();
    let partial = mining.tick(&world.scene, dt);
    view.particles = mining.particle_mesh(&world.atlas, forward, partial, light);

    // The mobs: a server tick when due, the blocks it changed, its hits on
    // the player, the mobs' boxes for bullets and their meshes.
    if let Some(entities) = entities.as_mut() {
        let player = crate::minecraft_entities::PlayerView {
            feet,
            alive,
            health: ps.health as f32,
            yaw: mc_yaw,
            pitch: ps.viewangles[0],
        };
        let bright_outside = world.environment.sky_light_level() > 11.0;
        let ticks_before = entities.client_ticks();
        let (changes, hits) = entities.tick(dt, day.ticks as i64, bright_outside, &player);
        let mob_ticks = (entities.client_ticks() - ticks_before) as u32;
        if !changes.is_empty() {
            let blocks = &world.registries.blocks;
            let mut positions = Vec::with_capacity(changes.len());
            for (pos, block) in changes {
                let state = block.as_ref().and_then(|b| world.stream.states.state_of(b));
                let shape = state.map_or(0, |state| {
                    *shapes.entry(state).or_insert_with(|| {
                        let boxes = blocks.collision_boxes(state);
                        if boxes.is_empty() {
                            return 0;
                        }
                        let key: Vec<[u32; 6]> = boxes.iter().map(|b| b.map(|v| (v as f32).to_bits())).collect();
                        if let Some(&id) = shape_ids.get(&key) {
                            return id;
                        }
                        let boxes32 = boxes.iter().map(|b| b.map(|v| v as f32)).collect();
                        let id = sim::voxel::add_shapes(vec![boxes32]).unwrap_or(0);
                        shape_ids.insert(key, id);
                        id
                    })
                });
                sim::voxel::set_block_shape(pos.0, pos.1, pos.2, shape);
                world.scene.set(pos, block);
                positions.push(pos);
            }
            world.stream.mark_edited(&world.scene, &positions);
            changed.extend(positions);
        }
        // The host logs each changed block's new state for its clients.
        if authority.is_some() && edit_log.generation == terrain.generation && !changed.is_empty() {
            let air = world.stream.states.state_of(&minecraft_terrain::scene::Block::new("minecraft:air"));
            for pos in changed.drain(..) {
                let state = minecraft_terrain::scene::Scene::block(&world.scene, pos)
                    .and_then(|block| world.stream.states.state_of(block))
                    .or(air);
                if let Some(state) = state {
                    edit_log.edits.push(([pos.0, pos.1, pos.2], state.0));
                }
            }
        }
        for (amount, from) in hits {
            sim::voxel::push_player_damage(local.0.0, amount, from.map(|b| sim::voxel::to_map(origin, b)));
        }
        // Items the host gave (a refused placement's block back).
        for grant in grants.read() {
            let _ = entities.inventory.add_item(
                minecraftoss_player::inventory::ItemStack::new(grant.item.clone(), grant.count),
                entities.selected,
            );
        }
        // The building kit, topped up once each life, after the guns have
        // taken the first hotbar slots.
        let armed = entities.inventory.slots.iter().flatten().any(|s| crate::minecraft_inventory::weapon_of(s).is_some());
        if settings.is_some_and(|s| s.kit) {
            if alive && armed && !*kit_given {
                for id in KIT {
                    let have: u32 = entities
                        .inventory
                        .slots
                        .iter()
                        .flatten()
                        .filter(|stack| stack.id == id)
                        .map(|stack| u32::from(stack.count))
                        .sum();
                    if have < 64 {
                        let _ = entities.inventory.add_item(
                            minecraftoss_player::inventory::ItemStack::new(id, (64 - have) as u8),
                            entities.selected,
                        );
                    }
                }
                *kit_given = true;
            } else if !alive {
                *kit_given = false;
            }
        }
        // The host applies its clients' placements it can accept, and gives
        // back the block of each it can't.
        if host {
            let snapshot = presented.snapshot();
            for (client, [x, y, z], state) in sim::voxel::take_place_requests() {
                let pos = (x, y, z);
                let block = world.stream.states.block(minecraftoss_core::BlockStateId(state)).cloned();
                let requester = snapshot.and_then(|s| s.players.iter().find(|(id, _)| id.0 == client)).map(|(_, p)| p);
                let reach = requester.filter(|p| p.pm_type == 0).is_some_and(|p| {
                    let eye = sim::voxel::to_block(origin, p.origin);
                    let d = [f64::from(x) + 0.5 - eye[0], f64::from(y) + 0.5 - (eye[1] + 1.62), f64::from(z) + 0.5 - eye[2]];
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() <= 7.0
                });
                let replaceable = minecraft_terrain::scene::Scene::block(&world.scene, pos).is_none_or(|b| {
                    matches!(
                        b.id.path.as_str(),
                        "water" | "short_grass" | "tall_grass" | "fern" | "large_fern" | "snow" | "seagrass" | "dead_bush"
                    )
                });
                match block {
                    Some(block)
                        if reach && replaceable && sim::voxel::inside_border(x, z) && !block_hits_player(pos, &players) =>
                    {
                        let shape = shape_for_state(Some(minecraftoss_core::BlockStateId(state)), &world.registries.blocks, shapes, shape_ids);
                        diag::info!(World, "Minecraft: client {client} placed {} at {pos:?}", block.id.key());
                        world.scene.set(pos, Some(block));
                        sim::voxel::set_block_shape(x, y, z, shape);
                        world.stream.record_edits(&world.scene, &[pos]);
                        world.stream.mark_edited(&world.scene, &[pos]);
                        entities.placed(&world.scene, pos);
                        changed.push(pos);
                    }
                    Some(block) => {
                        diag::info!(
                            World,
                            "Minecraft: refused client {client}'s {} at {pos:?} (reach {reach}, replaceable {replaceable}, in a player {})",
                            block.id.key(),
                            block_hits_player(pos, &players)
                        );
                        reliable.queue_mut(sim::ClientId(client)).push(net::ReliableRow::McGrant { item: block.id.key(), count: 1 });
                    }
                    None => {}
                }
            }
        }
        // The inventory: MW2 guns as items, the HUD's clicks, the hotbar's
        // gun, and what the HUD shows.
        let owned: Vec<u32> = ps
            .weapons
            .iter()
            .filter(|&&w| w > 0)
            .map(|&w| w as u32)
            .filter(|&w| {
                let facts = match (authority.as_ref(), prediction.as_ref()) {
                    (Some(authority), _) => authority.0.weapon_combat_row(w),
                    (None, Some(prediction)) => prediction.0.world().weapon_combat_row(w),
                    (None, None) => None,
                };
                facts.is_some_and(|facts| facts.inventory_type == 0)
            })
            .collect();
        ui.active = alive;
        if !alive {
            ui.inventory_open = false;
        }
        inventory_ui.sync_weapons(&mut entities.inventory, &owned);
        let mut selected = entities.selected;
        let thrown = inventory_ui.apply_input(&mut ui, &mut entities.inventory, &mut selected);
        let thrower = crate::minecraft_inventory::Thrower {
            eye: glam::DVec3::from_array(eye),
            yaw: mc_yaw,
            pitch: ps.viewangles[0],
        };
        crate::minecraft_inventory::throw(&mut entities.world_items, thrown, &thrower);
        ui.weapon_request = inventory_ui.weapon_request(&entities.inventory, &mut selected, ps.weapon as u32);
        entities.selected = selected;
        inventory_ui.publish(&mut ui, &entities.inventory, selected, &world.packs, &mut images);
        ui.hearts = hearts.clone();
        ui.health = alive.then_some((ps.health as f32, ps.max_health.max(1) as f32));

        if let Some(sounds) = sounds.as_mut() {
            for (event, position, volume, pitch) in std::mem::take(&mut entities.sounds) {
                sounds.play(&world.packs, &event, Some(at(position.to_array())), volume, pitch);
            }
        }
        // The held item in view; an empty hand is MW2's own hands.
        view.hand = Default::default();
        let swing = hand.swing.map_or(0.0, |t| (t / crate::minecraft_hand::SWING_TICKS).clamp(0.0, 1.0));
        ui.hand_swing = swing;
        if ui.holding_item
            && !puppet.active
            && let Some(stack) = entities.inventory.slots[entities.selected].clone()
        {
            let eye_light_at = glam::Vec3::new(eye[0] as f32, eye[1] as f32, eye[2] as f32);
            let display = minecraft_terrain::pack::ResourceId::parse(&stack.id)
                .ok()
                .and_then(|id| minecraft_terrain::model::item_first_person_transform(&world.packs, &id).ok())
                .unwrap_or(glam::Mat4::IDENTITY);
            let pose = crate::minecraft_hand::item_pose(display, swing, 0.0);
            let mesh = entities.held_item_mesh(&stack.id, pose, eye_light_at, &world.packs, &world.atlas, light);
            let vertices: Vec<minecraft_terrain::mesh::SectionVertex> =
                mesh.vertices.iter().map(minecraft_terrain::mesh::SectionVertex::from_vertex).collect();
            view.hand = (bytemuck::cast_slice(&vertices).to_vec(), mesh.indices);
            // Reverse-Z with no far plane, as the scene's; 70 degrees up.
            let f = 1.0 / (35.0f32.to_radians()).tan();
            let near = 0.05;
            view.hand_clip = Mat4::from_cols(
                Vec4::new(f / aspect, 0.0, 0.0, 0.0),
                Vec4::new(0.0, f, 0.0, 0.0),
                Vec4::new(0.0, 0.0, 0.0, -1.0),
                Vec4::new(0.0, 0.0, near, 0.0),
            )
            .to_cols_array();
        }

        sim::voxel::set_mob_boxes(entities.boxes());
        sim::voxel::set_mob_hostile(entities.hostile_keys());
        entities.tick_scene(&world.scene, mob_ticks);
        let sky_darken = (15.0 - world.environment.sky_light_level()).clamp(0.0, 15.0) as u8;
        let meshes = entities.meshes(
            &world.scene,
            &world.packs,
            &world.atlas,
            light,
            forward,
            glam::DVec3::from_array(eye),
            sky_darken,
        );
        let raw = |mesh: &minecraft_terrain::mesh::ChunkMesh| {
            (bytemuck::cast_slice::<_, u8>(&mesh.vertices).to_vec(), mesh.indices.clone())
        };
        view.entity_meshes = [
            raw(&meshes.models),
            raw(&meshes.culled),
            raw(&meshes.translucent),
            raw(&meshes.shadows),
        ];
        let mesh = meshes.items;
        let (bytes, indices) = &mut view.particles;
        let base = (bytes.len() / std::mem::size_of::<minecraft_terrain::mesh::SectionVertex>()) as u32;
        let vertices: Vec<minecraft_terrain::mesh::SectionVertex> =
            mesh.vertices.iter().map(minecraft_terrain::mesh::SectionVertex::from_vertex).collect();
        bytes.extend_from_slice(bytemuck::cast_slice(&vertices));
        indices.extend(mesh.indices.iter().map(|i| i + base));
    }
    view.cracks = mining.crack_mesh();
    day.advance(dt);
    let eye_block = (eye[0].floor() as i32, eye[1].floor() as i32, eye[2].floor() as i32);
    view.eye_light = [
        f32::from(light.get(eye_block)),
        f32::from(light.get_block(eye_block)),
    ];
    world
        .environment
        .update_rain_fog(0.0, light.get(eye_block), false, (dt * 20.0) as f32);
    *environment_accumulator += dt;
    if !*environment_primed || *environment_accumulator >= TICK_SECONDS {
        *environment_accumulator = (*environment_accumulator % TICK_SECONDS).min(TICK_SECONDS);
        let scene = &world.scene;
        world.environment.tick(
            day.ticks.floor() as i64,
            0.0,
            0.0,
            eye,
            |x, y, z| scene.noise_biome((x, y, z)).map_or(0, |id| id.0),
            !*environment_primed,
        );
        *environment_primed = true;
    }
    let partial_tick = (*environment_accumulator / TICK_SECONDS).clamp(0.0, 1.0) as f32;
    let sky = world.environment.sky_state(&View {
        partial_tick,
        forward,
        camera_y: eye[1] as f32,
        render_distance: VIEW_DISTANCE as u32,
        rain_level: 0.0,
        thunder_level: 0.0,
    });
    let render_distance = VIEW_DISTANCE as f32 * 16.0;
    let right = forward.cross(glam::Vec3::Y).normalize_or(glam::Vec3::X);
    let up = right.cross(forward).normalize_or(glam::Vec3::Y);
    let put = |v: glam::Vec3| [v.x, v.y, v.z, 0.0];
    let game_time = day.ticks;
    view.environment = [
        put(forward),
        put(right),
        put(up),
        [eye[0] as f32, eye[1] as f32, eye[2] as f32, 0.0],
        put(sky.sky),
        [sky.fog.x, sky.fog.y, sky.fog.z, render_distance.min(sky.sky_fog_end)],
        [
            sky.sky_light_color.x,
            sky.sky_light_color.y,
            sky.sky_light_color.z,
            sky.sky_light_factor,
        ],
        sky.sunset,
        [sky.sun_direction.x, sky.sun_direction.y, sky.sun_direction.z, sky.rain_brightness],
        [sky.moon_direction.x, sky.moon_direction.y, sky.moon_direction.z, sky.rain_brightness],
        // The brightness option at its default.
        [sky.cloud.x, sky.cloud.y, sky.cloud.z, 0.5],
        [aspect, 0.0, sky.star_brightness, sky.star_angle],
        [sky.moon_phase as f32, (game_time as f32) * 0.03, 96.0, 160.0],
        [
            sky.fog_start,
            sky.fog_end,
            render_distance - (render_distance / 10.0).clamp(4.0, 64.0),
            render_distance,
        ],
        [
            sky.ambient.x,
            sky.ambient.y,
            sky.ambient.z,
            match sky.skybox {
                Skybox::Overworld => 0.0,
                Skybox::End => 1.0,
                _ => 2.0,
            },
        ],
        [
            sky.block_light_tint.x,
            sky.block_light_tint.y,
            sky.block_light_tint.z,
            sky.block_factor,
        ],
    ];

    // Clouds, rebuilt when the camera crosses a cloud cell.
    if let Some(mask) = &world.cloud_mask {
        let center = mask.center(eye[0] as f32, eye[2] as f32, game_time);
        if *cloud_center != Some(center) {
            *cloud_center = Some(center);
            let mesh = mask.build(center, eye[1] as f32);
            view.clouds = Some(Arc::new((mesh.vertices, mesh.indices)));
        }
    }

    // The light MW2 models stand in, around the player.
    *light_volume_age += 1;
    let half = LIGHT_VOLUME / 2;
    let corner = [block.0 - half, block.1 - half, block.2 - half];
    let moved =
        light_volume_at.is_none_or(|at| (0..3).any(|k| (at[k] - corner[k]).abs() >= 4));
    if moved || *light_volume_age >= 20 {
        *light_volume_age = 0;
        *light_volume_at = Some(corner);
        let n = LIGHT_VOLUME;
        let index = |x: i32, y: i32, z: i32| (((y * n + z) * n + x) * 2) as usize;
        let mut raw = vec![0u8; (n * n * n * 2) as usize];
        for z in 0..n {
            for x in 0..n {
                let (bx, bz) = (corner[0] + x, corner[2] + z);
                // One column lookup for the whole stack, not two per cell.
                let column = light.chunk_column((bx >> 4, bz >> 4));
                for y in 0..n {
                    let pos = (bx, corner[1] + y, bz);
                    let at = index(x, y, z);
                    let (sky, block) = match column {
                        Some(column) => (column.get(pos), column.get_block(pos)),
                        None => (light.get(pos), light.get_block(pos)),
                    };
                    raw[at] = sky;
                    raw[at + 1] = block;
                }
            }
        }
        // Unlit cells (inside blocks) take their brightest neighbour, so a
        // model beside a block is not darkened by filtering into it. Levels
        // become unorm bytes.
        let mut data = vec![0u8; raw.len()];
        for y in 0..n {
            for z in 0..n {
                for x in 0..n {
                    let at = index(x, y, z);
                    let (mut sky, mut block) = (raw[at], raw[at + 1]);
                    if sky == 0 && block == 0 {
                        for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
                            let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                            if (0..n).contains(&nx) && (0..n).contains(&ny) && (0..n).contains(&nz) {
                                let near = index(nx, ny, nz);
                                sky = sky.max(raw[near]);
                                block = block.max(raw[near + 1]);
                            }
                        }
                    }
                    data[at] = sky.min(15) * 17;
                    data[at + 1] = block.min(15) * 17;
                }
            }
        }
        view.light_volume = Some(Arc::new((corner, data)));
    }
}

fn stop(runtime: &mut Runtime, view: &mut MinecraftWorldView) {
    runtime.awaiting_settings = false;
    runtime.settings = None;
    runtime.ready = false;
    runtime.spawn_check = None;
    runtime.chunk_parts = Default::default();
    runtime.chunks_waiting.clear();
    runtime.terrain_next = 0;
    runtime.edits_waiting.clear();
    runtime.next_edit = (0, 0);
    if runtime.world.take().is_some() || runtime.loading.take().is_some() || view.active {
        sim::voxel::deactivate();
        view.active = false;
        view.atlas = None;
        view.uploads.clear();
        view.removed.clear();
        view.visible.clear();
        view.celestial = None;
        view.crack_texture = None;
        view.particles = Default::default();
        view.entity_meshes = Default::default();
        view.hand = Default::default();
        view.cracks = Default::default();
        view.clouds = None;
        view.light_volume = None;
        view.generation += 1;
    }
}

/// `mc_save`, `mc_load` and `mc_saves` (`frame::McWorldCommand`).
fn world_command(
    runtime: &mut Runtime,
    command: &frame::McWorldCommand,
    reports: &mut MessageWriter<frame::McWorldReport>,
    exec: &mut MessageWriter<frame::UiExecCommand>,
) {
    let mut report = |line: String| {
        diag::info!(World, "Minecraft saves: {line}");
        reports.write(frame::McWorldReport(line));
    };
    match command {
        frame::McWorldCommand::List => {
            let names = crate::minecraft_saves::list();
            report(if names.is_empty() {
                "no Minecraft saves yet (mc_save <name>)".to_owned()
            } else {
                format!("Minecraft saves: {}", names.join(", "))
            });
        }
        frame::McWorldCommand::Save(name) => {
            let (Some(world), Some(entities), Some(feet)) =
                (runtime.world.as_mut(), runtime.entities.as_ref(), runtime.last_feet)
            else {
                report("mc_save: no Minecraft world is in play".to_owned());
                return;
            };
            world.stream.save_all();
            let mut meta = crate::minecraft_saves::SaveMeta::new(
                world.seed,
                runtime.day.ticks,
                runtime.difficulty.unwrap_or(minecraftoss_player::Difficulty::Normal),
                feet,
            );
            meta.selected = entities.selected;
            meta.slots = entities
                .inventory
                .slots
                .iter()
                .map(|slot| slot.as_ref().map(crate::minecraft_saves::SavedStack::of))
                .collect();
            match crate::minecraft_saves::write(name, world.stream.world_dir(), &meta) {
                Ok(regions) => report(format!(
                    "saved `{name}` (seed {}, {regions} region files) in {}",
                    world.seed,
                    crate::minecraft_saves::saves_dir().join(name).display()
                )),
                Err(error) => report(format!("mc_save: {error}")),
            }
        }
        frame::McWorldCommand::Time(value) => {
            let ticks = match value.to_ascii_lowercase().as_str() {
                "day" => Some(1000.0),
                "noon" => Some(6000.0),
                "night" => Some(13000.0),
                "midnight" => Some(18000.0),
                other => other.parse::<f64>().ok().filter(|t| (0.0..24000.0).contains(t)),
            };
            match (ticks, runtime.world.is_some()) {
                (Some(ticks), true) => {
                    // Keeps the day count, as `/time set` does.
                    let day = (runtime.day.ticks / 24000.0).floor() * 24000.0;
                    runtime.day.set(day + ticks);
                    report(format!("time set to {ticks}"));
                }
                (Some(_), false) => report("mc_time: no Minecraft world is in play".to_owned()),
                (None, _) => report(format!("mc_time: `{value}` is not day, noon, night, midnight or 0-23999")),
            }
        }
        frame::McWorldCommand::Slot(slot) => {
            if let Some(entities) = runtime.entities.as_mut() {
                entities.selected = usize::from(slot - 1);
                report(format!("hotbar slot {slot}"));
            }
        }
        frame::McWorldCommand::Use => runtime.scripted_use = true,
        frame::McWorldCommand::Load(name) => match crate::minecraft_saves::prepare_load(name) {
            Ok(save) => {
                report(format!("loading `{name}` (seed {})", save.0.seed));
                runtime.pending_load = Some(save);
                exec.write(frame::UiExecCommand {
                    text: format!("map {}", assets::minecraft_map::ZONE),
                });
            }
            Err(error) => report(format!("mc_load: {error}")),
        },
    }
}

/// What a Minecraft match's world is, chosen on the host and published to
/// clients as `mc_*` server info so they build the same one.
#[derive(Clone, Copy, Debug)]
struct WorldSettings {
    seed: i64,
    /// The block point at map origin, once the host's world is in.
    origin: Option<[f64; 3]>,
    /// The world border's half width in blocks; 0 is none.
    border: f64,
    mobs: bool,
    /// Every life starts with a building kit (else blocks come from mining).
    kit: bool,
    /// What the arena is made of.
    kind: WorldKind,
    difficulty: minecraftoss_player::Difficulty,
    /// The host's spawn-chunk checksum (`chunk_checksum`).
    check: Option<u64>,
}

impl WorldSettings {
    fn publish(&self, sim: &mut sim::SimWorld, day_ticks: f64) {
        let Some([x, y, z]) = self.origin else {
            return;
        };
        sim.set_server_info("mc_seed", &self.seed.to_string());
        sim.set_server_info("mc_origin", &format!("{x} {y} {z}"));
        sim.set_server_info("mc_border", &self.border.to_string());
        sim.set_server_info("mc_mobs", if self.mobs { "1" } else { "0" });
        sim.set_server_info("mc_blocks", if self.kit { "kit" } else { "survival" });
        sim.set_server_info("mc_world", self.kind.name());
        sim.set_server_info("mc_difficulty", &format!("{:?}", self.difficulty).to_ascii_lowercase());
        sim.set_server_info("mc_time", &(day_ticks.round() as i64).to_string());
        if let Some(check) = self.check {
            sim.set_server_info("mc_check", &format!("{check:016x}"));
        }
    }

    fn from_server_info(info: &[(String, String)]) -> Option<Self> {
        let get = |name: &str| info.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
        let seed = get("mc_seed")?.parse().ok()?;
        let origin: Vec<f64> = get("mc_origin")?.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        let [x, y, z] = origin[..] else {
            return None;
        };
        Some(Self {
            seed,
            origin: Some([x, y, z]),
            border: get("mc_border").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            mobs: get("mc_mobs") == Some("1"),
            kit: get("mc_blocks") == Some("kit"),
            kind: WorldKind::parse(get("mc_world").unwrap_or("natural")),
            difficulty: crate::minecraft_entities::parse_difficulty(get("mc_difficulty")),
            check: get("mc_check").and_then(|v| u64::from_str_radix(v, 16).ok()),
        })
    }
}

/// FNV-1a over a chunk's block states: the same on every machine that
/// generated the same chunk.
fn chunk_checksum(chunk: &minecraftoss_core::Chunk) -> u64 {
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

/// The arena's radius in chunks around the spawn chunk: what the border
/// encloses, with one chunk to spare; without a border, the spawn's
/// surroundings.
fn arena_radius(border: f64) -> i32 {
    if border > 0.0 {
        (border / 16.0).ceil() as i32 + 1
    } else {
        4
    }
}

/// The voxel shape id of a block state's collision, registering new shapes.
fn shape_for_state(
    state: Option<BlockStateId>,
    blocks: &minecraftoss_core::block::BlockRegistry,
    shapes: &mut HashMap<BlockStateId, u16>,
    shape_ids: &mut HashMap<Vec<[u32; 6]>, u16>,
) -> u16 {
    let Some(state) = state else {
        return 0;
    };
    *shapes.entry(state).or_insert_with(|| {
        let boxes = blocks.collision_boxes(state);
        if boxes.is_empty() {
            return 0;
        }
        let key: Vec<[u32; 6]> = boxes.iter().map(|b| b.map(|v| (v as f32).to_bits())).collect();
        if let Some(&id) = shape_ids.get(&key) {
            return id;
        }
        let boxes32 = boxes.iter().map(|b| b.map(|v| v as f32)).collect();
        let id = sim::voxel::add_shapes(vec![boxes32]).unwrap_or(0);
        shape_ids.insert(key, id);
        id
    })
}

/// The building kit each life starts with when the match gives one.
const KIT: [&str; 4] = ["minecraft:stone", "minecraft:oak_planks", "minecraft:glass", "minecraft:dirt"];

/// Whether a block would stand inside any of these players (feet in block
/// space; MW2's soldier is about 0.85 blocks wide and 2 tall).
fn block_hits_player((x, y, z): (i32, i32, i32), players: &[[f64; 3]]) -> bool {
    players.iter().any(|&[fx, fy, fz]| {
        (fx - 0.42) < f64::from(x + 1)
            && (fx + 0.42) > f64::from(x)
            && fy < f64::from(y + 1)
            && (fy + 1.95) > f64::from(y)
            && (fz - 0.42) < f64::from(z + 1)
            && (fz + 0.42) > f64::from(z)
    })
}

/// A spawn point in block space (the feet), on the surface inside the
/// border: team 1 on one side of the arena and team 2 on the other, everyone
/// else spread around it; `salt` varies the spot between spawns. None when
/// no candidate has ground.
fn spawn_point(world: &Loaded, origin: [f64; 3], border: f64, team: i32, salt: u64) -> Option<[f64; 3]> {
    let radius = if border > 0.0 { (border * 0.6).max(4.0) } else { 10.0 };
    let spread = (salt.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 33) as f64 / f64::from(1u32 << 31);
    let base = match team {
        1 => 0.0,
        2 => std::f64::consts::PI,
        _ => spread * std::f64::consts::TAU,
    };
    let limit = if border > 0.0 { border - 2.0 } else { f64::MAX };
    for attempt in 0..16 {
        // Nearby angles first, alternating sides, a little jitter for teams.
        let step = f64::from((attempt + 1) / 2) * if attempt % 2 == 0 { 1.0 } else { -1.0 };
        let angle = base + step * 0.35 + (spread - 0.5) * if team == 1 || team == 2 { 0.6 } else { 0.0 };
        let dx = (radius * angle.cos()).clamp(-limit, limit);
        let dz = (radius * angle.sin()).clamp(-limit, limit);
        let (x, z) = ((origin[0] + dx).floor() as i32, (origin[2] + dz).floor() as i32);
        if let Some(y) = surface(world, x, z, origin[1].floor() as i32) {
            return Some([f64::from(x) + 0.5, f64::from(y) + 1.0, f64::from(z) + 0.5]);
        }
    }
    None
}

/// The highest block near `around` in a column a player can stand on: solid
/// to collision, not a fluid, with two free blocks above.
fn surface(world: &Loaded, x: i32, z: i32, around: i32) -> Option<i32> {
    let solid = |y: i32| {
        minecraft_terrain::scene::Scene::block(&world.scene, (x, y, z)).is_some_and(|block| {
            !matches!(block.id.path.as_str(), "water" | "lava")
                && world
                    .stream
                    .states
                    .state_of(block)
                    .is_some_and(|state| !world.registries.blocks.collision_boxes(state).is_empty())
        })
    };
    let free = |y: i32| {
        minecraft_terrain::scene::Scene::block(&world.scene, (x, y, z)).is_none_or(|block| {
            !matches!(block.id.path.as_str(), "water" | "lava")
                && world
                    .stream
                    .states
                    .state_of(block)
                    .is_none_or(|state| world.registries.blocks.collision_boxes(state).is_empty())
        })
    };
    (around - 48..=around + 48).rev().find(|&y| solid(y) && free(y + 1) && free(y + 2))
}

/// What a Minecraft arena is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorldKind {
    /// Seeded generation.
    Natural,
    /// Superflat, built by the host.
    Flat,
    /// The stand-in MW2 map voxelized, built by the host.
    Replica,
    /// Seeded generation around its biggest village near the spawn.
    Village,
}

impl WorldKind {
    fn parse(name: &str) -> Self {
        match name.trim() {
            "flat" => Self::Flat,
            "replica" => Self::Replica,
            "village" => Self::Village,
            _ => Self::Natural,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Flat => "flat",
            Self::Replica => "replica",
            Self::Village => "village",
        }
    }
}
