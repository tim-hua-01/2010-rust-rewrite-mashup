# Minecraft-world multiplayer: design and plan

**Goal.** Play PvP (TDM, FFA) with friends in Minecraft worlds with Minecraft
destruction — shooting blocks out, blasts, building — both in ordinary
generated worlds and in block-for-block replicas of MW2 maps (Terminal, Rust,
...). Everyone sees the same terrain and the same broken and placed blocks;
each player has their own inventory.

**Decided** (2026-10-04):

| Question | Decision |
| --- | --- |
| Mobs in multiplayer | Off for now (a Game Setup toggle; off by default when hosting) |
| World size | Bounded arena with a world border (radius set in Game Setup) |
| World kinds | Generated worlds **and** MW2 map replicas |
| Cheating | Not a concern: friends only; inventories live on each player's machine |
| Connecting | Tailscale device sharing to start; the game is agnostic (a VPS later is a config change) |
| Platforms | Macs first; nothing Mac-only, so Windows builds work later |
| Building blocks | Game Setup option: **kit** (a stack of each building block per life, default) or **survival** (only what you mine) |

---

## Part 1 — How it works today

Verified by reading the code and by `make duo ZONE=minecraft:overworld`
(two game windows, host and client, through the local relay):

* **A joining client cannot spawn.** The Minecraft runtime
  (`crates/render_anim/src/minecraft_world.rs`, system `update`) starts the
  world load at `MatchInstalled`, then returns at `:402` when there is no
  `AuthorityWorld` — and clients don't have one
  (`crates/session/src/match_apply.rs:584-590`). So the load never completes,
  `ui.loading_world` stays true, and admission waits on it forever
  (`crates/session/src/admission.rs:33`). The duo client sat at
  "spawn: waiting for class select".
* **Every peer would pick its own seed** (`minecraft_world.rs:316-327`).
  Nothing in net or session carries the seed, the map-to-block origin
  (`view.origin`), the time of day or the difficulty; `HostMatchRules` never
  leaves the host.
* **Client prediction traces the wrong world.** `sim::voxel` is a
  process-global (`crates/sim/src/voxel.rs:40`) that traces use only when it
  was activated with the same clip-brush table pointer (`active_for`,
  `voxel.rs:466-471`). On a client it is never activated, so prediction traces
  the hidden stand-in map (`mp_rust`) while the host simulates blocks.
* **The host simulates one player.** Chunk streaming, voxel collision, the mob
  server, pickups and the inventory centre on `local.0`
  (`minecraft_world.rs:528-608`); `PLAYER = 0` is hard-coded in
  `minecraft_entities.rs:34` and `minecraft_terrain/src/server.rs:1145-1157`.
  A remote player far from the host traces empty space and falls.
* **Already shared by design:** the authority runs every player's weapons, so
  bullets, melee and blasts from anyone push voxel events on the host
  (`crates/sim/src/combat.rs:1235-1361`, `damage.rs:101`) and break blocks
  there — the host just never tells anyone. The `push_player_damage` queue
  (`voxel.rs:300-310`) is keyed by client.
* **The protocol has no open channel.** Snapshot meta, `ReliableRow`
  (`crates/net/src/transport/reliable.rs:41-57`) and `ClientAction`
  (`crates/sim/src/input.rs:12-108`) are closed enums with strict decoders,
  under `PROTOCOL_VERSION = 94` (`crates/net/src/lib.rs:172`), which must match
  between peers. Free-form carriers that exist: `objectives.server_info`
  key/value strings, sent with every snapshot including the join bootstrap;
  the bootstrap lane (≤ 256 KiB).

Useful facts about the transport (from `crates/net`): authority at 20 Hz;
snapshots are datagrams through the relay, zstd-compressed, fragmented into
~1084-byte pieces; reliable rows go over the relay's ordered control stream
(frames ≤ 16 KiB, ≤ 64 unacked rows per client — overflowing retires the
client); client actions go over the same control lane and come back with an
`ActionOutcome`.

---

## Part 2 — Design

### Principle: deterministic terrain, host-authoritative changes

Every peer generates the same terrain itself from the same inputs (world kind,
seed, replica map). Only **changes** travel: the host decides every change,
keeps them in an ordered **edit log**, and streams the log to clients. Clients
never simulate destruction themselves (mining progress and TNT rays use host
RNG); they apply the host's results.

```
            host (Listen)                                  client
  ┌───────────────────────────────┐            ┌───────────────────────────────┐
  │ world settings ── server_info ┼── snapshot ▶ load the same world           │
  │ bullets / blasts / placements │            │ voxel collision for prediction│
  │   └▶ mining, TNT rays          │            │                               │
  │       └▶ edit log (seq) ──────┼─ McEdits ──▶ apply in order: scene, voxel,  │
  │           full log on join ───┼─ bootstrap ▶ re-mesh, particles, sound      │
  │ validate + apply ◀────────────┼─ McPlace ──┤ right click (block in hand)   │
  │ credit breaker ───────────────┼─ McGrant ──▶ inventory += item             │
  └───────────────────────────────┘            └───────────────────────────────┘
```

### D1. Match world settings

A `McWorldSettings` value describes the world everyone must build:

| Field | Source | Notes |
| --- | --- | --- |
| `kind` | Game Setup | `natural` (seeded generation), `flat` (superflat), `replica:<mw2 map>` |
| `seed` | host | random per match, or a loaded save's |
| `origin` | host | `view.origin`, the block point at map origin (spawn search / save) |
| `border` | Game Setup | radius in blocks around the arena centre (default 64) |
| `difficulty` | Game Setup | already exists (`scr_mc_difficulty`) |
| `mobs` | Game Setup | on/off; default off when hosting a lobby, on solo |
| `blocks` | Game Setup | `kit` or `survival` |
| `day_ticks` | host | refreshed every few seconds so clients' sky follows |

Host rules travel as `scr_mc_*` menu dvars (as difficulty does today). The
host publishes the settings to clients as `mc_*` keys in
`objectives.server_info` — **no protocol change** — e.g.
`mc_world=natural`, `mc_seed=4987…`, `mc_origin=-52.5 67 -21.5`,
`mc_border=64`, `mc_mobs=0`, `mc_blocks=kit`, `mc_time=6184`.

### D2. Runtime split: shared world vs host authority

`minecraft_world.rs` `update` is split by role:

* **World presence (every peer):** load the world once settings are known,
  stream terrain around the local player, mesh it, install `sim::voxel`
  chunks, light, sky, minimap, hand, sounds, HUD (hotbar, hearts, inventory),
  apply edits.
* **Authority (host only):** turn voxel events into edits (mining, blasts),
  validate placements, append to the edit log, run mobs when enabled, credit
  breakers, spawn placement, the border.

Load trigger: the host loads at `MatchInstalled` (as now); a client loads when
the first snapshot carrying `mc_seed` arrives. Admission (`admission.rs:33`)
keeps waiting on `loading_world`, which now completes on clients too.

### D3. Collision on clients

A client activates `sim::voxel` with **its prediction world's** clip-brush
table (`ClientPredictionState`'s content), so `active_for` matches its
prediction traces. On a listen host the authority and prediction already share
one content `Arc`, so one activation covers both (as today).

### D4. The host covers the whole arena

The host must have collision wherever any player is. With a border, the host
tracks chunks around the **arena centre** (not its own player) with a view
distance covering the border radius (`VIEW_DISTANCE` derived from `border`,
e.g. 64 blocks → 5 chunks + margin). Every player is inside the border, so
the host always has their ground. Clients still stream around themselves for
rendering. (Unbounded worlds would need a tracking view per player in
MinecraftOSS's `ChunkMap`; out of scope.)

### D5. The world border

Enforced in collision so host and client agree without new messages: when
`sim::voxel` is active, block columns outside `border` trace as solid
(`shape_at` returns a full box beyond the radius). Players simply can't walk
out; bullets stop there too. Visual: a faint wall drawn at the border
(phase 4). Blocks outside the border can't be edited.

### D6. The edit log

* `McEdit { pos: [i32; 3], state: u32 }` — `state` is the `BlockStateId`,
  identical on every peer with the same Minecraft 26.3 files; 0 is air.
* The host appends every change it applies, from every source: mining
  (`minecraft_mining.rs:148-157`), blasts, placements, server/creeper changes
  (`minecraft_world.rs:920-945`). One sequence number per edit.
* A **compacted view** (latest state per position) is what joiners receive and
  what saves store.
* Clients apply edits strictly in sequence (a gap waits): scene `set`,
  `voxel::set_block_shape`, `record_edits`/`mark_edited` for re-meshing and
  lighting, break particles and the break sound.

### D7. Protocol additions (`PROTOCOL_VERSION` 94 → 95)

| Addition | Lane | Purpose |
| --- | --- | --- |
| `ReliableRow::McEdits { first_seq: u32, edits: Vec<McEdit> }` | reliable control | live edits, batched per tick, split so a frame stays < 16 KiB (~900 edits) |
| `ReliableRow::McGrant { item: u16, count: u8 }` | reliable control | "you broke this; add it to your inventory" (survival mode) |
| bootstrap `McWorld` transaction | bootstrap uni stream | the compacted log for a joiner (falls back to chunked `McEdits` past 256 KiB) |
| `ClientAction::McPlace { pos: [i32; 3], state: u32 }` | control (actions) | ask the host to place a block |
| `ClientAction::McMine { pos: [i32; 3] }` | control (actions) | hand-mining progress tick (punching) |
| attacker on `VoxelEvent::Shot` / `Ray` / `Explosion` | in-process | who to credit for a break |

Item ids in `McGrant` use a small table (block items only) shared by all peers
from the same data files.

Back-pressure: a big blast can produce hundreds of edits; the host batches per
tick and caps rows per tick so a client's 64-row reliable queue never
overflows (overflow retires the client today).

### D8. Placing, punching, inventories

* Inventories stay **client-side** (trusted). Right click with a block sends
  `McPlace`; the host checks reach (~6 blocks from the requester's eye), that
  the target is replaceable, inside the border, and not inside any player's
  box (all players, not just the requester), then applies it through the
  edit log. The client removes the item when the edit comes back; a rejected
  `ActionOutcome` leaves the inventory alone. No local prediction in v1
  (round trip on Tailscale ≈ 20–80 ms).
* **Kit mode:** each spawn refills the hotbar with stacks of building blocks
  (stone, planks, glass, dirt) besides the guns.
* **Survival mode:** the host credits the player whose bullet, blast or punch
  broke a block with an `McGrant`; dropped item entities stay off in
  multiplayer.

### D9. Mobs

Off in multiplayer v1: the host doesn't run the mob server's natural spawning,
clients don't run it at all, `sim::voxel` mob lists stay empty. Later: the host
runs mobs with every player as a candidate (`EntityWorld` is keyed by player
id already; `ServerSim` hard-codes player 0 in about a dozen places) and a
compact mob list (kind, position, yaw, pose flags) rides snapshot meta.

### D10. Spawns and TDM

Today every Minecraft spawn is map origin (`crates/sim/src/spawn.rs:244`).
The host generates spawn points on the surface inside the border — two team
areas on opposite sides for TDM, scattered points for FFA — and feeds them to
the spawn logic. Replicas use the MW2 map's own spawn points.

### D11. World kinds

* **natural** — today's seeded overworld, inside the border.
* **flat** — superflat (grass/dirt/bedrock), a clean PvP floor; MinecraftOSS
  generation with a flat preset.
* **replica:<map>** — the MW2 map's clip brushes (already parsed for any map:
  `install_clip_and_player`) voxelized into a flat world: each brush's volume
  becomes blocks, the material chosen from its surface type (stone/concrete →
  stone or smooth stone, wood → planks, glass → glass, metal → iron blocks,
  dirt/grass → dirt/grass, water → water), at 36 map units per block (MW2's
  70-unit player is ~2 blocks, as now). Deterministic on every peer from the
  map's own files; the border fits the map's bounds; spawns are the map's.
  Generated once per match and cached under `iw4l-artifacts/replicas/`.

### D12. Saves in multiplayer

Region files bake edits in, so a client can't rebuild a loaded save from the
seed alone. Saves gain the compacted edit list (`edits.bin`) and the world
settings; loading one on a host seeds its edit log, and clients receive it like
any join.

### D13. Connecting: Tailscale

* The relay (`iw4l-master`) runs on the host's Mac bound to its Tailscale
  address; friends' `.env` points at it with the shared CA file.
* No router changes, nothing on a public address; friends install Tailscale
  (personal account), accept a device share, and run the friend kit.
* **Friend kit** (`scripts/mc-friend-setup.sh`): asks for the relay address,
  writes `IW4L_MASTER_ADDR/SERVER_NAME/CA_CERT` into `.env`, installs the CA
  file. **Host kit** (`scripts/mc-host-relay.sh`): starts/stops the relay on
  the Tailscale address. Both documented in `MAC.md`.

---

## Part 3 — Plan

Each phase ends with a commit series on `minecraft-features`, an automated
`make duo` check (two windows through the local relay, scripted with
`HOST_CMDS`/`CLIENT_CMDS`, screenshots and log assertions), and a note in
`MAC.md`. Sizes: S ≈ an hour or two of work, M ≈ an afternoon, L ≈ a day+.

### Phase 1 — Everyone in the same world (M)

1. Publish `mc_*` world settings in `server_info` from the host
   (`minecraft_world.rs`; the sim's `objectives.server_info` writer).
2. Client: start the world load from received settings instead of
   `MatchInstalled`; keep `loading_world` true until it's in.
3. Split `update` into world presence (all peers) and authority (host); the
   client path runs without `AuthorityWorld`.
4. Client voxel activation against the prediction content (`ClientPredictionState`).
5. Mobs off in hosted lobbies: Game Setup toggle `scr_mc_mobs`, default off
   when hosting; clients never run the mob server.
6. Spawn placement for everyone, not just `local.0` (ground check per player).

**Done when:** duo client spawns, both windows render the same terrain (same
seed in both logs, matching screenshots at a fixed spot), the client walks on
blocks without rubber-banding (prediction error logs stay quiet), players see
and can shoot each other.

### Phase 2 — Destruction syncs (M–L)

1. Edit log on the host fed by every edit source; compacted view.
2. `ReliableRow::McEdits` encode/decode; per-tick batching and caps;
   `PROTOCOL_VERSION` 95.
3. Client apply path (scene, voxel, re-mesh, particles, sound), strict order.
4. Join catch-up: bootstrap `McWorld` transaction (chunked fallback).
5. Host streams the arena (D4) so remote shooters' bullets always find blocks.

**Done when:** in duo, blocks the client shoots out disappear in both
windows; a blast crater matches in both; a client that joins after edits sees
them; the client can walk into a hole dug by the host.

### Phase 3 — Building and inventories (M)

1. `ClientAction::McPlace` / `McMine`; host validation; `ActionOutcome`.
2. Attacker ids on voxel events; `ReliableRow::McGrant`; survival crediting.
3. Kit mode: building blocks on every spawn; Game Setup `scr_mc_blocks`.
4. Per-player inventories on each client; HUD unchanged.

**Done when:** in duo, a block placed by the client appears for both and is
solid for both; placing inside a player is refused; survival grants the
breaker the block; kit refills on respawn.

### Phase 4 — Arenas and TDM (L)

1. World border in collision (D5), radius from `scr_mc_border`; border wall
   visual.
2. Team/FFA spawn generation inside the border (D10).
3. `flat` world kind.
4. `replica:<map>` world kind: voxelize clip brushes with surface-type
   materials; spawns from the map; cache; Game Setup picks the map. Start with
   one map (Rust or Terminal), then generalize.

**Done when:** a TDM match on a flat arena and on a replica map spawns teams
apart, the border holds, and destruction works on the replica's walls.

### Phase 5 — Friends (S, plus testing)

1. Relay on the Tailscale address; host and friend kits; `MAC.md` steps.
2. A session with a friend over Tailscale.

### Later

Mobs in multiplayer; multiplayer saves (D12); Windows build for friends
(`make setup-windows`, `cargo-xwin`); unbounded worlds (per-player chunk
tracking); predicted block placement.

---

## Risks and how we'll catch them

| Risk | Mitigation |
| --- | --- |
| Terrain generation not bit-identical across machines (threads, float math, Windows) | Debug check: host and client log checksums of a few chunks near spawn; duo compares them |
| A big blast floods a client's reliable queue (64 rows) | Batch per tick, cap rows per tick, carry the remainder to the next tick |
| Prediction disagreements near freshly edited blocks | Apply edits to the client's voxel world as soon as they arrive; the normal reconciliation corrects the rest |
| Replica voxelization looks wrong (thin walls vanish at 36-unit blocks) | Conservative fill: any block a brush overlaps is solid; a thin-wall pass for brushes thinner than a block |
| Protocol bump | Every player already needs the same build; the version check fails loudly otherwise |

## Open questions

* Team colours/markers in replicas and on the minimap.
* Killcam and demo playback on Minecraft maps (the killcam replays snapshots;
  edits would need replaying too).
* Bots in Minecraft worlds (they use the MW2 map's navigation).
