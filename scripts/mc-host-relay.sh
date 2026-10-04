#!/usr/bin/env bash
# Runs the relay (iw4l-master) on this Mac's Tailscale address so friends on
# your tailnet (or a device you shared with them) can join your matches, and
# points this Mac's game at it.
#
#   scripts/mc-host-relay.sh start   # start (makes certificates the first time)
#   scripts/mc-host-relay.sh stop
#   scripts/mc-host-relay.sh status
set -euo pipefail
cd "$(dirname "$0")/.."
CA_DIR="$HOME/.iw4l/ca"
PORT=4433
LOG="iw4l-artifacts/logs/relay.log"

tailscale_ip() {
    command -v tailscale >/dev/null || { echo "Tailscale isn't installed: https://tailscale.com/download" >&2; exit 1; }
    tailscale ip -4 2>/dev/null | head -1
}

make_certs() {
    [ -f "$CA_DIR/server-cert.pem" ] && return
    echo "Making the relay's certificates in $CA_DIR"
    mkdir -p "$CA_DIR" && chmod 700 "$CA_DIR"
    (cd "$CA_DIR"
     openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out ca-key.pem 2>/dev/null && chmod 600 ca-key.pem
     openssl req -x509 -new -sha256 -days 3650 -key ca-key.pem -out iw4l-ca.pem -subj "/CN=IW4L Release CA"
     openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out server-key.pem 2>/dev/null && chmod 600 server-key.pem
     openssl req -new -key server-key.pem -out server.csr -subj "/CN=iw4l-prod"
     printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:iw4l-prod,DNS:iw4l-dev\n' > server.ext
     openssl x509 -req -sha256 -days 825 -in server.csr -CA iw4l-ca.pem -CAkey ca-key.pem -CAserial ca.srl -CAcreateserial -extfile server.ext -out server-cert.pem 2>/dev/null
     rm server.csr server.ext)
}

set_env() {
    local key="$1" value="$2"
    touch .env
    if grep -q "^$key=" .env; then
        sed -i '' "s|^$key=.*|$key=$value|" .env
    else
        echo "$key=$value" >> .env
    fi
}

case "${1:-}" in
start)
    [ -x target/play/iw4l-master ] || cargo build --locked --profile play -p iw4l-master
    ip=$(tailscale_ip)
    [ -n "$ip" ] || { echo "Tailscale isn't connected (tailscale up)" >&2; exit 1; }
    make_certs
    pkill -f "iw4l-master serve" 2>/dev/null || true
    mkdir -p "$(dirname "$LOG")"
    nohup ./target/play/iw4l-master serve --bind "$ip:$PORT" \
        --cert "$CA_DIR/server-cert.pem" --key "$CA_DIR/server-key.pem" > "$LOG" 2>&1 &
    sleep 1
    set_env IW4L_MASTER_ADDR "$ip:$PORT"
    set_env IW4L_MASTER_SERVER_NAME iw4l-prod
    set_env IW4L_MASTER_CA_CERT "$CA_DIR/iw4l-ca.pem"
    echo "Relay running on $ip:$PORT (log: $LOG)."
    echo
    echo "Send each friend:"
    echo "  1. the relay address:   $ip:$PORT"
    echo "  2. the file:            $CA_DIR/iw4l-ca.pem   (it is public; it lets them trust your relay)"
    echo "Then host with Create Game; friends join with Find Lobbies."
    ;;
stop)
    pkill -f "iw4l-master serve" && echo "Relay stopped." || echo "No relay running."
    ;;
status)
    if pgrep -f "iw4l-master serve" >/dev/null; then
        addr=$(grep '^IW4L_MASTER_ADDR=' .env | cut -d= -f2)
        ./target/play/iw4l-master status --connect "$addr" --server-name iw4l-prod --ca-cert "$CA_DIR/iw4l-ca.pem"
    else
        echo "No relay running."
    fi
    ;;
*)
    echo "usage: $0 start|stop|status" >&2
    exit 1
    ;;
esac
