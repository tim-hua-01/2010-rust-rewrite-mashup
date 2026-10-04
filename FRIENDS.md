# Playing together

One person hosts: they run the relay and create the match. Everyone else joins
it. Everyone needs an Apple silicon Mac and their own Steam copy of Modern
Warfare 2 (2009). Connections go over [Tailscale](https://tailscale.com), so
nobody has to touch their router.

## Host (once)

1. Install Tailscale, sign in, and invite each friend to your tailnet
   (admin console -> Users -> Invite), or share just this Mac with them
   (Machines -> this Mac -> Share).
2. Build the game and fetch MW2 as in [Everyone](#everyone-once) below.

## Host (each session)

```bash
scripts/mc-host-relay.sh start
```

It starts the relay on your Tailscale address, points your game at it, and
prints what to send each friend: the relay address and `~/.iw4l/ca/iw4l-ca.pem`
(a public certificate; it lets their game trust your relay). Then
`./target/play/iw4l menu`, **Create Game**, and in **Game Setup** pick the map
(Minecraft tab: `overworld`, `rust`, `terminal`), Team Deathmatch or
Free-for-All, and the Minecraft rows (world border, natural or flat world,
mobs, blocks). `scripts/mc-host-relay.sh stop` when you're done.

## Everyone (once)

1. Install the Xcode command line tools and Rust:

   ```bash
   xcode-select --install
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. Get the code and build it (takes a while; the fans will spin):

   ```bash
   git clone -b minecraft-features https://github.com/tim-hua-01/2010-rust-rewrite-mashup
   cd 2010-rust-rewrite-mashup
   cargo build --locked --profile play -p launcher -p iw4l-master
   ```

3. Download MW2's files with steamcmd (`brew install --cask steamcmd`). It asks
   for your Steam password and Steam Guard code itself:

   ```bash
   steamcmd +@sSteamCmdForcePlatformType windows +force_install_dir ~/Games/MW2 \
     +login <your steam name> +app_update 10190 +quit
   ```

4. Friends only: install Tailscale, accept the host's invite, then:

   ```bash
   scripts/mc-friend-setup.sh <relay address from the host> <path to iw4l-ca.pem>
   ```

## Joining

`./target/play/iw4l menu`, then **Find Lobbies** and pick the host's lobby. The
first start downloads Minecraft's files from Mojang (about 125 MB). Your game
receives the host's world, so everyone fights on the same blocks, and every
block someone breaks or places shows up for everyone.

Keep the lid open and the screen unlocked while playing. Controls, console
commands and the rest are in [MAC.md](MAC.md).
