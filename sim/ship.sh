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
HOST=vincenzopalazzo@debian
REMOTE_DIR='$HOME/lampo-sim'
BUNDLES='$HOME/bundles'

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
echo "bundle: $BUNDLE ($(du -h "$BUNDLE" | cut -f1))"

scp -q "$BUNDLE" "$HOST:/tmp/ship.bundle"
# Keep the harness usable on the server regardless of which branch is
# checked out (fix branches do not contain sim/): synced out-of-repo.
rsync -a --delete "$(git rev-parse --show-toplevel)/sim/" "$HOST:lampo-sim-harness/"
ssh "$HOST" 'chmod +x ~/lampo-sim-harness/simulate.sh ~/lampo-sim-harness/ship.sh 2>/dev/null || true'
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
echo "shipped $BRANCH ($SHA)"
