#!/usr/bin/env bash
#
# ship.sh <branch> — ship a branch from this worktree to a remote build host
# as an incremental git bundle, then (on the host) fetch + checkout + build.
#
# Use when the build host has no GitHub credentials. Requires LAMPO_HOST.
#
# Usage:
#   LAMPO_HOST=user@host ./simulations/ship.sh main
#   LAMPO_HOST=user@host ./simulations/ship.sh fix/some-bug --no-build
#
# Env:
#   LAMPO_HOST        required (e.g. user@regtest-host)
#   LAMPO_REMOTE_DIR  remote clone path (default: $HOME/lampo-sim)
#   LAMPO_BUNDLES     remote bundle dir (default: $HOME/bundles)
#   LAMPO_HARNESS_DIR out-of-repo harness copy name (default: lampo-sim-harness)
#   LAMPO_HARNESS_SYNC  set 0 to skip rsync of simulations/ (default: 1)
set -euo pipefail

BRANCH=${1:?usage: LAMPO_HOST=user@host ship.sh <branch> [--no-build]}
NOBUILD=0; [ "${2:-}" = "--no-build" ] && NOBUILD=1
HOST=${LAMPO_HOST:?set LAMPO_HOST=user@regtest-host}
REMOTE_DIR=${LAMPO_REMOTE_DIR:-'$HOME/lampo-sim'}
BUNDLES=${LAMPO_BUNDLES:-'$HOME/bundles'}
HARNESS_DIR=${LAMPO_HARNESS_DIR:-'lampo-sim-harness'}

cd "$(git rev-parse --show-toplevel)"
git rev-parse --verify "$BRANCH" >/dev/null

SHA=$(git rev-parse "$BRANCH")
BUNDLE="/tmp/lampo-ship-${BRANCH//\//-}-$(date +%Y%m%d%H%M%S).bundle"
if ssh "$HOST" "ls $BUNDLES/lampo-init.bundle >/dev/null 2>&1"; then
  git bundle create "$BUNDLE" --remotes --branches "$BRANCH" >/dev/null
else
  git bundle create "$BUNDLE" "$BRANCH" >/dev/null
fi
scp -q "$BUNDLE" "$HOST:/tmp/ship.bundle"
ssh "$HOST" bash -s "$BRANCH" "$NOBUILD" "$REMOTE_DIR" "$BUNDLES" <<'REMOTE'
set -euo pipefail
BRANCH=$1; NOBUILD=$2; REMOTE_DIR=$(eval echo $3); BUNDLES=$(eval echo $4)
mkdir -p "$BUNDLES"
if [ ! -d "$REMOTE_DIR/.git" ]; then
  git clone /tmp/ship.bundle "$REMOTE_DIR"
  (cd "$REMOTE_DIR" && git remote rename origin bundles)
else
  (cd "$REMOTE_DIR" && git fetch /tmp/ship.bundle "+refs/heads/*:refs/remotes/bundles/*")
fi
cd "$REMOTE_DIR"
git checkout -B "$BRANCH" "bundles/$BRANCH" 2>/dev/null || git checkout -B "$BRANCH" FETCH_HEAD
echo "server: $REMOTE_DIR on $(git rev-parse --short HEAD) ($BRANCH)"
cp /tmp/ship.bundle "$BUNDLES/lampo-latest.bundle"
if [ ! -f "$BUNDLES/lampo-init.bundle" ]; then cp /tmp/ship.bundle "$BUNDLES/lampo-init.bundle"; fi
if [ "$NOBUILD" = 0 ]; then
  echo "server: building release (nohup, log: $REMOTE_DIR/build.log)"
  nohup bash -c 'source $HOME/.cargo/env 2>/dev/null; cd '"$REMOTE_DIR"' && cargo build --release && echo BUILD_OK || echo BUILD_FAIL' \
      > "$REMOTE_DIR/build.log" 2>&1 &
  echo "server: build started; watch: ssh $HOST tail -f $REMOTE_DIR/build.log"
fi
REMOTE
if [ "${LAMPO_HARNESS_SYNC:-1}" = 1 ]; then
  if ssh "$HOST" 'command -v rsync >/dev/null'; then
    rsync -a --delete "$(git rev-parse --show-toplevel)/simulations/" "$HOST:$HARNESS_DIR/"
  else
    ssh "$HOST" "mkdir -p ~/$HARNESS_DIR"
    scp -qr "$(git rev-parse --show-toplevel)/simulations/" "$HOST:$HARNESS_DIR/"
  fi
  ssh "$HOST" "chmod +x ~/$HARNESS_DIR/*.sh 2>/dev/null || true"
fi
echo "shipped $BRANCH ($SHA)"
