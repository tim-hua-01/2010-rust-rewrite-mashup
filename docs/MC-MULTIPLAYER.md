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

---

## Review feedback (2026-10-03)

The overall architecture is a good fit for friends-only multiplayer:
deterministic base terrain, host-authoritative changes, and trusted local
inventories. The recommendations below tighten the implementation contracts
and phase dependencies. They are review proposals, not additional decisions.
They come from reading this plan and the relevant code; this review did not
run `make duo` or reproduce the runtime observations in Part 1.

Priority: **P1** means resolve before implementing the affected protocol or
runtime path; **P2** means address in the phase breakdown or acceptance checks.

### F1. P1 — Define the handoff from join state to live edits (D2, D6, D7)

A compacted list is world state, not a contiguous edit history. If the host
has applied 1,000 edits to 100 positions, sending those 100 positions cannot
satisfy a client waiting for sequences 1 through 1,000. Edits made while the
join state is being prepared or terrain is loading also need an explicit
ordering boundary. The bootstrap and control streams must not be assumed to
arrive in order relative to each other.

Suggested contract:

1. Capture an immutable compacted view at edit sequence **S**. Its header
   identifies the match epoch/world, world settings, and `through_seq = S`.
   The records describe the latest state at each edited position through S;
   they do not pretend to be the original edit sequence.
2. Retain and deliver every live edit after S for that joining client. Buffer
   live edits that arrive before the base world and compacted view are ready.
3. Install the compacted view, set `next_seq = S + 1`, and drain the buffered
   contiguous suffix. Ignore duplicates already applied; reject stale epochs.
4. Admit the player only after settings, spawn-area collision, the complete
   compacted view, and a declared catch-up watermark are installed. Define
   that watermark so continuous destruction cannot keep moving the readiness
   target indefinitely.
5. Give chunked fallback transfers an identity, part count or completion
   marker, and the same S. A missing part or sequence must have a timeout and
   explicit retry/resync/failure behavior rather than waiting forever.

The current `BootstrapTransaction` in `crates/net/src/transport/bootstrap.rs`
describes a snapshot offer. `flush_bootstrap_applied` in
`crates/net/src/client/runtime.rs` acknowledges after snapshot adoption;
Minecraft readiness is not part of that acknowledgement today. Decide how
`McWorld` extends that transaction or supplies a separate readiness gate.
Also distinguish waiting for settings from having a load thread running:
`loading_world = runtime.loading.is_some()` alone would become false while a
new client is still waiting for the host's settings.

**Acceptance:** join while the host repeatedly edits the same positions and
continues blasting throughout loading. Both peers converge to the same
compacted state; no missing-sequence wait occurs. Disconnect/rejoin and change
maps while a transfer is pending; old-world records never reach the new world.
Applying join state produces no historical break sounds or particles.

### F2. P1 — Persist edits independently of loaded chunks (D2, D6)

There is a concrete hazard in the current apply primitives:

* `HandcraftedScene::set(pos, None)` in
  `crates/minecraft_terrain/src/scene.rs` only records a cleared position when
  `generated_block(pos)` exists. Clearing an ungenerated block leaves no air
  marker.
* `sim::voxel::set_block_shape` only updates an already loaded voxel chunk.
* `ChunkMap::set_blocks` in
  `third_party/minecraftoss/world/src/chunk_map.rs` ignores edits for chunks
  absent from its current chunk set.

Consequently, applying D6's calls once when a network edit arrives is not
sufficient. A far-away destruction edit can disappear when the client later
generates that chunk. A placement can remain in the scene overlay while the
new chunk's collision is built from unedited generated states.

Keep a persistent per-chunk overlay of the latest authoritative state at each
edited position, including explicit air. Store received edits even when no
terrain is loaded. When a chunk arrives, apply its overlay before publishing
collision, lighting, or meshes. Chunk eviction must not discard the overlay;
world teardown must clear it. For loaded chunks, update the overlay and all
representations through one apply path. Snapshot installation should batch
remeshing and lighting work rather than schedule it once per historical edit.

**Acceptance:** receive air and placement edits before generation, then load
the chunk and check both rendering and collision. Walk far enough to unload
it, return, and repeat. Include negative chunk coordinates and section-edge
edits so indexing and neighboring mesh invalidation are exercised.

### F3. P1 — Budget queue capacity and complete control frames (D7)

A per-tick row cap limits production rate; it does not bound accumulated
unacknowledged rows. At one new row per tick, a client whose acknowledgements
stall can still fill the 64-row queue. Other events and action outcomes share
that queue.

There is also a separate byte limit. `PeerReplication::queue_control` in
`crates/net/src/transport/udp_session.rs` encodes multiple fresh reliable rows
into one `ServerPacket::Control`. `ControlFrame::Relay` in
`crates/master_protocol/src/lib.rs` checks the complete payload against
`MAX_CONTROL_BYTES - HEADER_BYTES`. Two individually valid large edit rows
can exceed that limit together.

Suggested implementation:

* Check each client's outstanding reliable count before enqueueing, reserving
  capacity for outcomes and other game events. Keep its unsent edit cursor
  outside the reliable queue and advance it only after successful enqueue.
* Split complete encoded control packets by byte budget, including all row,
  packet, and relay headers. Derive edit batch size from the encoder, not the
  approximate "900 edits" figure.
* Define bounds for retained edit history and join buffers. If a client falls
  behind that retained range, start a new compacted catch-up or retire it with
  a clear reason. Never skip arbitrary live edits to make space.
* Log outstanding rows, pending edit bytes, and edit lag per client. A slow
  client should not block healthy peers or make host memory grow indefinitely.

**Acceptance:** stall acknowledgements during sustained explosions and an
oversized join transfer. No attempted enqueue overflows the reliable queue;
every encoded frame fits the transport limit; a healthy peer keeps receiving
edits. Resume acknowledgements and verify convergence, or exercise the
documented resync/failure policy when the backlog bound is exceeded.

### F4. P1 — Tie inventory mutations to action outcomes (D7, D8)

`McEdit { pos, state }` does not identify the requester or placement request.
"Remove the item when the edit comes back" cannot reliably distinguish the
client's placement from somebody else's edit at the same position. Slot
changes, concurrent requests, and rejected placements make this ambiguous.
The existing `ReliableInbound::apply` consumes `ActionOutcome` and retires
the action without notifying an inventory handler.

Use the existing action request ID convention for `McPlace` and `McMine`.
For placement, reserve one item against that request, preserving its identity
even if the selected slot changes. An applied outcome consumes the reservation
once; a refused outcome releases it once. The edit stream updates the world
independently. Expose outcomes to the Minecraft inventory layer and define
reservation behavior across death, kit refill, disconnect, and world teardown.

On the host, record `Applied` only after validation and the authoritative edit
commit. `record_action_outcomes` currently infers success from the absence of
a recognized refusal event; Minecraft refusals must participate in that path,
or the new actions need an explicit result path. Reuse action-ledger
deduplication so retries cannot apply the placement or consume the item twice.
Listen-host placements should use the same validation and commit path.

Use one authoritative mutation function for mining, blasts, placements, and
server changes: update world state, append the edit sequence, and produce
credit/effects as appropriate. This makes it harder for a new edit source to
change terrain without being replicated. Keep historical catch-up silent and
distinguish placement effects from break effects. For `McMine`, award hand
progress at the host's tick cadence; a burst of delayed requests must not
become several simultaneous mining ticks merely because they arrive together.

**Acceptance:** switch slots while placement is pending; send two placements
with one item remaining; have two players place into the same cell; resend an
identical request; reject placement inside a player; die before the outcome.
Each accepted request consumes exactly one reserved item, each refusal
consumes none, and host/client input produces the same result. Survival credit
is issued once to the breaker chosen by the host's processing order.

### F5. P2 — Establish bounded collision before multiplayer movement (D4, D5, D10)

Phase 2's whole-arena coverage assumes players stay inside a border, but border
enforcement currently arrives in Phase 4. Before then, a remote player can
leave the loaded collision area. Move authoritative arena coverage and the
collision border to Phase 1; keep the visual wall in Phase 4 if desired.

Specify the arena centre, border shape, supported radius range, and conversion
between map and block coordinates. Derive coverage with margin for player
boxes and neighboring generation/light work, and wait for necessary collision
before spawning players. The current `set_view_distance` couples server
tracking to render sections; make authority coverage independent of a host's
render-distance preference so a graphics change cannot remove remote ground.

Spawn generation is also not a one-time safety guarantee. Destruction can
remove a spawn's floor and building can obstruct it. Validate ground, headroom,
and border clearance on every spawn/respawn, then choose another candidate
or a defined fallback. Replace the existing voxel-active origin shortcut in
`crates/sim/src/spawn.rs` when introducing generated and replica spawn points.

**Acceptance:** place the remote player near the border, far from the host,
and verify walking, skating, shooting, and placement against the loaded world.
Change the host's render preference without losing remote collision. Destroy
or obstruct spawn candidates and verify respawns remain inside the arena on
valid ground.

### F6. P2 — Separate replica feasibility from arena/TDM delivery (D11, Phase 4)

Brush voxelization is a useful starting point, but it does not establish a
block-for-block map replica. `install_clip_and_player` installs brushes,
triangle meshes, and static-model collision separately in
`crates/session/src/match_apply.rs`. A brush-only conversion omits the latter
geometry, while collision-only data does not capture every visible detail.
Choose whether the first deliverable is a playable collision-derived remake
or a closer visual replica, and estimate those scopes separately.

Conservative overlap filling preserves thin walls but can thicken opposing
walls until doorways, stairs, and passages become unusable. Prototype Rust
first and inspect recognizable geometry, important routes, player clearance,
spawn accessibility, and destructible surfaces before generalizing to Terminal.
Decide which collision contents count as physical geometry; trigger and
invisible clipping volumes should not automatically become visible buildings.

Define a stable replica origin/transform and cache identity. Include source
map content, voxel scale, voxelizer version, and material mapping in the cache
key so algorithm changes do not silently reuse old worlds. A replica border
must fit the selected map rather than inherit a too-small natural-world radius.
Keep flat arenas and TDM as one milestone and replica conversion as a separate
prototype/milestone, then revise the estimate using that result.

**Acceptance:** the Rust prototype has agreed recognizable features, usable
routes and spawns, and matching world/collision checksums on both peers.
Destroy and rebuild a wall, then join late. Changing the voxelizer or mapping
invalidates the cache and both peers still choose the same base world.

### Suggested phase changes and additional validation

Keep the existing phase structure, with these dependency changes:

| Phase | Recommended adjustment |
| --- | --- |
| 1 | Add bounded authority collision coverage, collision border, explicit settings/loading readiness, and a base-world compatibility check. |
| 2 | Specify snapshot watermark/epoch and admission handoff first; implement persistent chunk overlays and queue/frame budgets with edit replication. |
| 3 | Add item reservations, outcome delivery to inventory, and explicit host action results before enabling placement. |
| 4 | Deliver safe respawns, flat arenas, TDM, and the border visual; split replica feasibility into its own milestone. |
| 5 | Test a real remote connection with the same convergence and backlog checks used locally. |

Keep duo screenshots and prediction logs, but also compare block-state
checksums and selected collision probes after edits and catch-up. Compare base
terrain at the same generation stage, and edited terrain at the same edit
watermark, to avoid treating legitimate in-flight changes as divergence.
Include a data/generator identity in world compatibility; protocol version
alone does not prove matching Minecraft data or replica caches. A known base
world mismatch should fail visibly instead of letting peers play on different
terrain. Same-machine duo tests do not establish cross-machine determinism.

For actual TDM acceptance, define score/time-limit behavior: Minecraft setup
currently forces those limits to zero in `crates/session/src/match_apply.rs`.
For v1, explicitly choose whether Minecraft killcams and demos are disabled
or accepted with current terrain; accurate historical terrain needs edit
replay. These choices should be stated before calling the PvP milestone done.

---

## Finding (2026-10-04): base terrain is not deterministic

Measured with `cargo run --profile play -p minecraft_terrain --example
determinism -- iw4l-artifacts/minecraft-26.3 <seed> <runs> <radius>`, which
generates one seed several times and checksums every chunk around a fixed
point:

| Setup | Chunks differing (49, radius 3, 4 runs) |
| --- | --- |
| default threads, streaming | 41 |
| 1 generation thread, streaming | 12–37 |
| 1 thread, pre-generated in a fixed order, seeded spawn search | 37 |

The differences are feature decorations that span chunk borders (lush-cave
moss, grass, azalea, dripleaf; some deepslate/clay). Even single-threaded in a
fixed order the output varies, so it is not only thread scheduling; something
in MinecraftOSS's generation depends on per-run state. A two-window duo run
also caught it live (`mc_check` mismatch).

**Consequence:** D1's "every peer generates the same terrain from the seed" does
not hold. Peers would walk on different blocks (collision disagreement).

**Options:**

1. **Host sends the arena's terrain** (recommended). With a border the arena is
   bounded (64-block half width ≈ 9×9 chunks); the host serializes those chunks'
   block states (palette + zstd, a few KB per chunk, roughly 0.3–0.6 MB) and
   streams them to joiners over the reliable lane with flow control, before
   edits. Outside the border clients may generate their own scenery (cosmetic
   only, never collided with). Works for replicas, saves and Windows alike, and
   reuses the join-transfer machinery Phase 2 needs anyway (F1, F3).
2. **Make MinecraftOSS generation deterministic.** Unknown effort: find the
   per-run state in feature placement and cross-chunk decoration. Vanilla
   Minecraft is itself generation-order dependent.

The spawn-chunk checksum (`mc_check`) stays as a safety net either way.

## Progress

* **Phase 1 done (2026-10-04)** with option 1 above: the host sends the arena's
  terrain. Host encodes each arena chunk (region-file NBT, zlib, ~8.6 KB) into
  `frame::McTerrainSource`, nearest the spawn first; `net` streams them as
  `ReliableRow::McChunk` pieces (≤ 10 KB, 8 per tick per client, at most 32
  outstanding rows), control packets split under the relay limit; protocol 95.
  A client holds the arena (`TerrainStream::set_held_area`): arena columns show
  only the host's chunk, solid to collision until it arrives; it spawns once
  the spawn chunk and its neighbours are in. Duo verified: the client's spawn
  chunk checksum matches the host's, it spawns, and its walk ends at the same
  authoritative position on both sides; the border stops players; Game Setup
  shows difficulty, border and mobs rows. Remote players still spawn at the
  stand-in map's spawn points (Phase 4 replaces them).
* **Phase 2 done (2026-10-04):** the host logs every block change (bullets,
  blasts, its own placing, mob-server changes) as `(pos, BlockStateId)` in
  `frame::McEditLog`; `net` sends a new client the compacted state
  (`ReliableRow::McEdits`, `first_seq` 0) then every later edit in order, on the
  same ordered lane and window as terrain, edits first. A client stores edits
  in the scene overlay even for chunks not loaded (`set_authoritative`), so a
  chunk arriving later gets them; chunk loads lay the overlay over collision.
  Duo verified: 1,272 live edits applied with contiguous sequences; the client
  fell into the shaft the host dug. Not yet exercised with a non-empty join
  snapshot (duo connects both windows at start).
