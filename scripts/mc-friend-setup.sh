#!/usr/bin/env bash
# Points this Mac's game at a friend's relay.
#
#   scripts/mc-friend-setup.sh <relay address, e.g. 100.112.109.11:4433> <path to iw4l-ca.pem> [MW2 games folder]
set -euo pipefail
cd "$(dirname "$0")/.."
[ $# -ge 2 ] || { echo "usage: $0 <relay host:port> <iw4l-ca.pem> [games folder, default ~/Games]" >&2; exit 1; }
addr="$1" ca="$2" games="${3:-$HOME/Games}"
[ -f "$ca" ] || { echo "No such file: $ca" >&2; exit 1; }
[ -f "$games/MW2/iw4mp.exe" ] || echo "Warning: no $games/MW2/iw4mp.exe; download MW2 first (see FRIENDS.md)." >&2
mkdir -p "$HOME/.iw4l/friend"
cp "$ca" "$HOME/.iw4l/friend/iw4l-ca.pem"
cat > .env <<ENV
IW4L_GAMES=$games
IW4L_MASTER_ADDR=$addr
IW4L_MASTER_SERVER_NAME=iw4l-prod
IW4L_MASTER_CA_CERT=$HOME/.iw4l/friend/iw4l-ca.pem
ENV
echo "Done. Start the game with ./target/play/iw4l menu and pick Find Lobbies."
