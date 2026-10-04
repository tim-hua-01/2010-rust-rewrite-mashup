# Playing on a Mac

Everything runs from this folder:

```bash
cd ~/Documents/funprojs/2010-rust-rewrite-mashup
```

## Launch

```bash
./target/play/iw4l menu                       # main menu
./target/play/iw4l map minecraft:overworld    # straight into the Minecraft world
./target/play/iw4l map mp_rust                # straight into an MW2 map
./target/play/iw4l map minecraft:village      # the biggest village near spawn, walled in
./target/play/iw4l map minecraft:rust         # Rust rebuilt in blocks
./target/play/iw4l map minecraft:terminal     # Terminal rebuilt in blocks
```

Straight into the Minecraft world with settings:

```bash
IW4L_MINECRAFT_DIFFICULTY=hard ./target/play/iw4l map minecraft:overworld
IW4L_MINECRAFT_SEED=12345 ./target/play/iw4l map minecraft:overworld   # a fixed world
IW4L_MINECRAFT_TIME=13000 ./target/play/iw4l map minecraft:overworld   # start at night
```

From the menu: **Create Game**, then **Game Setup** for the map (Minecraft tab,
`overworld`), the mode (Free-for-All or Team Deathmatch; objective modes
misbehave there) and **MINECRAFT DIFFICULTY**.

Keep the lid open and the screen unlocked: a locked screen stalls the game and
closing the lid quits it.

## Rebuild after changing code

```bash
cargo build --locked --profile play -p launcher -p iw4l-master
```

## Console (the ` key, under Esc)

| Command | Does |
| --- | --- |
| `mc_time day` / `noon` / `night` / `midnight` / `<ticks>` | Set the time of day |
| `mc_save <name>` | Save the world: blocks, time, difficulty, your place and inventory |
| `mc_load <name>` | Load a save (the map loads again) |
| `mc_saves` | List saves (`iw4l-artifacts/minecraft-saves/`) |
| `mcbind <key> <mc_inventory\|mc_drop\|mc_hotbar1-9>` / `mcunbind <key>` | Minecraft keys |
| `bind <key> <command>` / `unbind <key>` | MW2 keys |
| `give ammo`, `give killstreak/uav` | MW2 cheats |
| `bot add 3` | Add bots |
| `force_match_start` | Skip the pre-match wait |
| `map minecraft:overworld` | Load a map |

## Controls

Change them in **Options -> Controls** (Movement, Actions, Look, and
**Minecraft**): pick a row, press the new key; Esc cancels. On the Minecraft
map a key in the Minecraft page wins over the MW2 action on the same key.

Minecraft defaults: E inventory, Q drop, 1-9 hotbar, mouse wheel hotbar, left
click mine/punch, right click place, J skate.

## Heartbeat sensor

Pick a class whose primary has the heartbeat attachment (Create a Class), then
select that gun: hostile mobs ping red on the gun's sensor screen, other mobs
as friendlies.

## Multiplayer

See [FRIENDS.md](FRIENDS.md). In short: `scripts/mc-host-relay.sh start` runs
the relay on this Mac's Tailscale address and prints what to send friends;
`scripts/mc-host-relay.sh stop` stops it. The relay's certificates live in
`~/.iw4l/ca`; only `iw4l-ca.pem` is meant to be shared.
