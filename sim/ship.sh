#!/usr/bin/env bash
#
# ship.sh <branch> — ship a branch from this worktree to the debian server
# as an incremental git bundle, then (on the server) fetch + checkout + build.
#
# The server has no GitHub SSH key, so git bundles over scp are the transport.
#
# Usage:
#   ./sim/ship.sh sim/main            # ship current sim/main
#   ./sim/ship.sh fix/some-bug        # ship a fix branch for regression
#   ./sim/ship.sh sim/main --no-build # fetch+checkout only
#
# Server layout: ~/lampo-sim (clone whose origin is the bundle dir ~/bundles)
set -euo pipefail

BRANCH=${1:?usage: ship.sh <branch> [--no-build]}
NOBUILD=0; [ "${2:-}" = "--no-build" ] && NOBUILD=1
HOST=${LAMPO_HOST:-vincenzopalazzo@debian}
# Which server clone receives the branch: the sim/test-573-577 soak uses the
# default ~/lampo-sim; the main-next soak + interop tiers use ~/lampo-main-sim.
REMOTE_DIR=${LAMPO_REMOTE_DIR:-'$HOME/lampo-sim'}
BUNDLES=${LAMPO_BUNDLES:-'$HOME/bundles'}
# Out-of-repo copy of sim/ so the harness works regardless of checkout.
# Default keeps the legacy behavior; main-next ships to its own clone's sim/.
HARNESS_DIR=${LAMPO_HARNESS_DIR:-'lampo-sim-harness'}

cd "$(git rev-parse --show-toplevel)"
git rev-parse --verify "$BRANCH" >/dev/null

SHA=$(git rev-parse "$BRANCH")
BUNDLE="/tmp/lampo-ship-${BRANCH//\//-}-$(date +%Y%m%d%H%M%S).bundle"
# Full history bundle if it's the first ship (server has no ~/bundles yet),
# otherwise incremental from what the server already knows.
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
  nohup bash -c 'source $HOME/.cargo/env; cd '"$REMOTE_DIR"' && cargo build --release && echo BUILD_OK || echo BUILD_FAIL' \
      > "$REMOTE_DIR/build.log" 2>&1 &
  echo "server: build started; watch with: ssh $USER@debian tail -f $REMOTE_DIR/build.log"
fi
REMOTE
# Harness sync AFTER the clone (a pre-existing dir breaks `git clone`).
# Keeps the harness usable regardless of which branch is checked out
# (fix branches do not contain sim/): synced out-of-repo.
# Set LAMPO_HARNESS_SYNC=0 to skip when the target branch ships sim/ itself
# (e.g. sim/main-next) — syncing into the clone would delete the checkout.
if [ "${LAMPO_HARNESS_SYNC:-1}" = 1 ]; then
  if ssh "$HOST" 'command -v rsync >/dev/null'; then
    rsync -a --delete "$(git rev-parse --show-toplevel)/sim/" "$HOST:$HARNESS_DIR/"
  else
    ssh "$HOST" "mkdir -p ~/$HARNESS_DIR"
    scp -qr "$(git rev-parse --show-toplevel)/sim/" "$HOST:$HARNESS_DIR/"
  fi
  ssh "$HOST" "chmod +x ~/$HARNESS_DIR/*.sh 2>/dev/null || true"
fi
echo "shipped $BRANCH ($SHA)"
