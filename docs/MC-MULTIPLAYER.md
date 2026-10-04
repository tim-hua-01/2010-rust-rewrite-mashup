# Minecraft-world multiplayer: design

Goal: PvP (TDM, FFA) with friends in Minecraft worlds with Minecraft
destruction (shooting blocks out, blasts, building), later on Minecraft
replicas of MW2 maps. Everyone sees the same terrain and the same broken and
placed blocks; each player has their own inventory. Mobs are secondary.
Macs first; nothing in the design may be Mac-only.

## Where things stand

Verified by reading the code and by `make duo ZONE=minecraft:overworld`:

* **A joining client cannot spawn.** The whole Minecraft runtime
  (`render_anim/src/minecraft_world.rs` `update`) returns before its world
  installs when there is no `AuthorityWorld` (`:402`), and clients drop it
  (`session/src/match_apply.rs:584-590`). Its `ui.loading_world` stays true,
  which holds admission (`session/src/admission.rs:33`) — the duo client sat at
  "spawn: waiting for class select".
* **Every peer picks its own seed** (`minecraft_world.rs:316-327`). Nothing in
  net or session carries the seed, the map-to-block origin, the time of day or
  the difficulty (`HostMatchRules` never leaves the host).
* **Client prediction traces the wrong world.** `sim::voxel` is a
  process-global activated with the authority's clip-brush pointer; a client
  predicts against the stand-in `mp_rust` brushes while the host simulates
  blocks.
* **The host simulates one player.** Chunk streaming, voxel collision, the mob
  server, pickups and the inventory centre on `local.0`; `PLAYER = 0` is
  hard-coded in `minecraft_entities.rs` and `minecraft_terrain/src/server.rs`.
  Remote players beyond ~8 chunks from the host would fall through the world.
* **What already works for everyone:** bullets, melee and blasts from any
  player push voxel events on the authority (`sim/src/combat.rs:1235-1361`,
  `damage.rs:101`), so the host already breaks blocks for remote shooters; it
  just never tells anyone. The `push_player_damage` queue is per client.
* **The protocol has no open channel.** Snapshot meta, `ReliableRow`,
  `ClientAction` are closed enums with strict decoders and a
  `PROTOCOL_VERSION` (94) that must match. Free-form carriers that exist:
  `objectives.server_info` key/value strings on every snapshot, and the
  bootstrap lane (≤256 KiB) on join.

## Shape of the solution

**Host-authoritative edits over deterministic terrain.** Every peer
generates the same terrain from the host's seed itself (MinecraftOSS
generation is seeded and deterministic); only *changes* travel. The host
decides every change; clients render and collide against generated terrain
plus the host's change log.

```
host                                        client
seed, origin, time ── server_info ────────▶ load world (same seed), voxel on
bullets/blasts/places ─▶ edit log ─ reliable McEdits rows ─▶ apply edits
                         full log ─ bootstrap on join ──────▶ catch up
◀── ClientAction::McPlace / McMine (control lane) ── right click / punch
```

### 1. World identity (no protocol change)

Host publishes `mc_seed`, `mc_origin` (x,y,z), `mc_day_ticks` and
`mc_difficulty` in `objectives.server_info`; they ride every snapshot,
including the join bootstrap. A client starts its world load when the seed
arrives instead of at `MatchInstalled`, and admission waits for it as it
already does for the host.

### 2. Client world and collision

* Clients run the Minecraft runtime without `AuthorityWorld`: stream terrain,
  build meshes and activate `sim::voxel` with **their prediction world's**
  brush pointer, so prediction traces the same blocks as the host.
* Each peer streams around its own player for rendering and collision. The
  host additionally needs collision wherever any player is. v1 keeps matches
  inside one arena (see §6) so the host's view covers everyone; v2 gives
  MinecraftOSS's `ChunkMap` a tracking view per player.

### 3. Edit replication (protocol 95)

* An edit is `(block x,y,z: i32, state: u32)` — `BlockStateId`, identical on
  every peer with the same Minecraft 26.3 files; 0 is air. ~16 bytes.
* Host appends every applied change (mining, blasts, placing, creeper/server
  changes) to an ordered **edit log** with a sequence number.
* Live: a new `ReliableRow::McEdits { first_seq, edits }`, batched per tick
  (control frames hold 16 KiB ≈ 1000 edits; a big blast splits across rows).
  Reliable, ordered, resent until acked — the existing per-client queue.
* Join: the full log rides the bootstrap lane as a second transaction (or
  chunked `McEdits` rows when larger than 256 KiB). Clients apply edits only
  in sequence; a gap waits.
* Clients apply an edit as the host does: scene, `voxel::set_block_shape`,
  section re-mesh, break particles/sound — but never re-simulate mining or
  TNT rays (those use host RNG).

### 4. Placing, mining by hand, inventories (protocol 95)

* New `ClientAction::McPlace { pos, face, state }` and `McMine { pos }` on the
  reliable control lane. Host validates (reach, not inside any player's box,
  target replaceable) and applies through the same edit path; the verdict
  returns as the existing `ActionOutcome`.
* Inventories stay **client-side** in v1 (trusting friends): the hotbar,
  pickup of what you break (host sends `McGrant { item, count }` to the player
  credited with a break — needs an attacker id on `VoxelEvent::Shot`/blast).
  A host-authoritative inventory can come later if cheating matters.

### 5. Mobs

v1: **off in multiplayer** (Peaceful, no spawning) — they're simulated around
the host only and replicating them is the largest piece of work. v2: host runs
the mob server with every player as a candidate (`EntityWorld` is already
keyed by player id; `ServerSim` hard-codes 0 in a dozen places) and a compact
mob list (kind, position, yaw, pose flags) rides snapshot meta.

### 6. Arenas, spawns, TDM

* Every spawn on a Minecraft map is map origin today (`sim/src/spawn.rs:244`)
  — TDM needs spawn points: generate team spawns on the surface around the
  arena centre, far apart.
* **Arena bounds**: a world border (radius in blocks, a host rule) keeps
  players inside what the host streams, and makes small PvP maps.
* **MW2 map replicas**: voxelize an MW2 map's own clip brushes (already loaded
  for the stand-in map) into blocks — stone for walls, planks for floors,
  glass for windows — over a flat world, with its real spawn points. This is
  a world-generation step, so it needs no networking beyond the seed and the
  map name.

### 7. Saves in multiplayer

Region files bake edits in, so a client can't reproduce a loaded save from the
seed. Saves gain an explicit edit list (`edits.bin`); loading one replays it
into the host's edit log, which then reaches clients like any other edits.

### 8. Hosting and connecting

* Hosting stays **Create Game**; joining is **Find Lobbies**. The relay
  (`iw4l-master`) is the only shared piece.
* A `friend kit`: one script that writes the relay address and the CA file
  into the player's `.env`, and a `host-relay` script for whoever runs it.
* Relay location is a deployment choice (Tailscale share, port forward or a
  small VPS); the game doesn't care.
* Windows: same protocol; build with the repo's cross-compile setup
  (`make setup-windows`, `cargo-xwin`) or on Windows.

## Phases

| Phase | Delivers | Size |
| --- | --- | --- |
| 0 | Relay reachable by friends; friend kit | small |
| 1 | Shared world: seed/origin/time via server_info, client loads world, voxel collision on client prediction, admission fix, mobs off in MP | medium |
| 2 | Edit log + `McEdits` rows + join catch-up: destruction syncs | medium-large |
| 3 | `McPlace`/`McMine` actions, block grants, inventories per player | medium |
| 4 | Team spawns, world border, MW2 map voxel replicas | medium-large |
| 5 | Mobs in MP, saves in MP, Windows build | large |

Every phase is tested with `make duo` (two windows through the local relay)
before friends.

## Risks

* **Terrain determinism across machines.** Generation must match bit for bit
  on every peer (and on Windows). Mitigation: a terrain checksum per chunk in
  debug builds; host and client compare a few.
* **Protocol bump** means every player runs the same build — already true
  (the protocol is unversioned across builds).
* **Large blasts** produce hundreds of edits in one tick; batching and the
  reliable queue's 64-row limit need care (a dropped queue retires a client).
