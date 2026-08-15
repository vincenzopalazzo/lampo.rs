#!/usr/bin/env bash
#
# sim/ldk-deploy.sh — provision LDK-Server nodes on the debian server for the
# lampo interop/simln tiers. Run ON THE SERVER (or via ssh).
#
#   ./sim/ldk-deploy.sh build            # clone + protoc + cargo build (once)
#   ./sim/ldk-deploy.sh start [N]        # write configs, launch lk1..lkN
#   ./sim/ldk-deploy.sh stop [N]         # SIGTERM lk1..lkN
#   ./sim/ldk-deploy.sh status           # show node info of all running
#
# Env: LDK_REPO(~/ldk-server) LDK_REF(main) LDKDIR($LDK_REPO/ldk-nodes)
#      LDK_GRPC_BASE(3540) LDK_P2P_BASE(9840)
#      CORE_URL CORE_USER CORE_PASS (same regtest bitcoind as the lampo sims)
set -uo pipefail

LDK_REPO=${LDK_REPO:-$HOME/ldk-server}
LDK_REF=${LDK_REF:-main}
LDKDIR=${LDKDIR:-$LDK_REPO/ldk-nodes}
LDK_GRPC_BASE=${LDK_GRPC_BASE:-3540}
LDK_P2P_BASE=${LDK_P2P_BASE:-9840}
CORE_URL=${CORE_URL:-http://127.0.0.1:18332}
CORE_RPC_HOSTPORT=${CORE_RPC_HOSTPORT:-127.0.0.1:18332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
BIN=$LDK_REPO/target/release/ldk-server
CLI=$LDK_REPO/target/release/ldk-server-cli
HOSTPORT=(${CORE_URL//:/ })           # http 127.0.0.1 18332

say() { echo "[ldk-deploy $(date +%H:%M:%S)] $*"; }

grpc_port() { echo $(( LDK_GRPC_BASE + $1 )); }
p2p_port()   { echo $(( LDK_P2P_BASE + $1 )); }

ensure_protoc() {
  command -v protoc >/dev/null 2>&1 && { protoc --version; return 0; }
  if sudo -n true 2>/dev/null; then
    say "installing protobuf-compiler via apt"
    sudo -n apt-get update -qq && sudo -n apt-get install -y -qq protobuf-compiler && return 0
  fi
  say "no passwordless sudo — installing user-local protoc to ~/.local/protoc"
  local v=29.3 zip=/tmp/protoc.zip
  curl -sSL -o "$zip" \
    "https://github.com/protocolbuffers/protobuf/releases/download/v$v/protoc-$v-linux-x86_64.zip"
  mkdir -p "$HOME/.local/protoc"
  python3 - "$zip" "$HOME/.local/protoc" <<'PY'
import sys, zipfile
zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])
PY
  export PROTOC="$HOME/.local/protoc/bin/protoc"
  "$PROTOC" --version
}

do_build() {
  if [ ! -d "$LDK_REPO/.git" ]; then
    git clone https://github.com/lightningdevkit/ldk-server.git "$LDK_REPO"
  fi
  ( cd "$LDK_REPO" && git fetch --tags origin && git checkout "$LDK_REF" \
      && git reset --hard origin/"$LDK_REF" 2>/dev/null; \
    say "ldk-server at $(git rev-parse --short HEAD) ($(git log -1 --format=%ci))" )
  ensure_protoc
  source "$HOME/.cargo/env" 2>/dev/null || true
  say "building ldk-server + cli (release) — log: $LDK_REPO/build.log"
  nohup bash -c "source $HOME/.cargo/env 2>/dev/null; \
    export PROTOC='${PROTOC:-}'; cd '$LDK_REPO' && \
    cargo build --release -p ldk-server -p ldk-server-cli && echo BUILD_OK || echo BUILD_FAIL" \
    > "$LDK_REPO/build.log" 2>&1 &
  say "started; watch: tail -f $LDK_REPO/build.log"
}

write_conf() { # $1 = idx
  local dir=$LDKDIR/lk$1
  mkdir -p "$dir"
  cat > "$dir/config.toml" <<EOF
[node]
network = "regtest"
listening_addresses = ["127.0.0.1:$(p2p_port "$1")"]
announcement_addresses = ["127.0.0.1:$(p2p_port "$1")"]
grpc_service_address = "127.0.0.1:$(grpc_port "$1")"
alias = "lk$1"
pathfinding_scores_source_url = ""

[storage.disk]
dir_path = "$dir/data"

[log]
level = "Debug"
log_to_file = true

[bitcoind]
rpc_address = "$CORE_RPC_HOSTPORT"
rpc_user = "$CORE_USER"
rpc_password = "$CORE_PASS"
EOF
  echo "$dir/config.toml"
}

api_key_of() { # $1 = idx -> hex api key
  local f
  for f in "$LDKDIR/lk$1/data/regtest/api_key" "$LDKDIR/lk$1/data/api_key"; do
    [ -f "$f" ] && { od -An -tx1 -v "$f" | tr -d ' \n'; return 0; }
  done
  return 1
}

ldk_cli() { # ldk_cli <idx> <subcmd...>
  local i=$1; shift
  local key; key=$(api_key_of "$i") || { echo "no api_key for lk$i"; return 1; }
  "$CLI" --base-url "127.0.0.1:$(grpc_port "$i")" --api-key "$key" \
         --tls-cert "$LDKDIR/lk$i/data/tls.crt" "$@"
}

do_start() {
  local n=${1:-2} i
  for i in $(seq 1 "$n"); do
    write_conf "$i" >/dev/null
    local dir=$LDKDIR/lk$i
    setsid nohup "$BIN" "$dir/config.toml" > "$dir/console.log" 2>&1 < /dev/null &
    disown 2>/dev/null || true
  done
  for i in $(seq 1 "$n"); do
    for _ in $(seq 1 30); do
      api_key_of "$i" >/dev/null 2>&1 && ldk_cli "$i" get-node-info >/dev/null 2>&1 && break
      sleep 3
    done
    say "lk$i up: $(ldk_cli "$i" get-node-info 2>&1 | head -c 300)"
  done
}

do_stop() {
  local n=${1:-99} i
  for i in $(seq 1 "$n"); do
    pkill -f "ldk-server $LDKDIR/lk$i/config.toml" 2>/dev/null && say "lk$i stopped"
  done
  true
}

do_status() {
  local i=1
  while [ -d "$LDKDIR/lk$i" ]; do
    if pgrep -f "ldk-server $LDKDIR/lk$i/config.toml" >/dev/null; then
      echo "lk$i RUNNING grpc=:$(grpc_port "$i") p2p=:$(p2p_port "$i") — $(ldk_cli "$i" get-node-info 2>&1 | head -c 200)"
    else
      echo "lk$i down"
    fi
    i=$((i+1))
  done
}

case "${1:-}" in
  build)  do_build ;;
  start)  do_start "${2:-2}" ;;
  stop)   do_stop "${2:-99}" ;;
  status) do_status ;;
  *) echo "usage: $0 build|start|stop|status [N]"; exit 1 ;;
esac
