#!/usr/bin/env bash
#
# sim/run-mh.sh <soak|recover|stop> — the ONLY sanctioned way to start/stop
# the mh-cluster harnesses on the server. Makes concurrent instances
# structurally impossible:
#   1. kills every mh harness + mh node by exact data-dir patterns
#   2. VERIFIES nothing survived (retries) before wiping/starting
#   3. refuses to run if another run-mh.sh is active (own pidfile)
# Lesson (2026-08-15): a leftover endless multihop soak ran concurrently
# with a recovery run against the same SIMDIR and produced an
# unattributable broken-node state (issue #578 amendment).
set -uo pipefail
CMD=${1:?usage: run-mh.sh <soak|recover|stop> [extra env as VAR=..]}
RUNLOCK=/tmp/lampo-run-mh.lock
SIMDIR=${SIMDIR:-$HOME/lampo-sim/sim-run}
HARNESS=${HARNESS:-$HOME/lampo-sim-harness}

if [ -f "$RUNLOCK" ] && kill -0 "$(cat "$RUNLOCK" 2>/dev/null)" 2>/dev/null; then
  echo "another run-mh.sh is active (pid $(cat "$RUNLOCK")) — refusing"; exit 3
fi
echo $$ > "$RUNLOCK"; trap 'rm -f "$RUNLOCK"' EXIT

stop_all() {
  local pids i
  for i in 1 2 3 4 5; do
    pids=$(pgrep -f "bash .*multihop\.sh|bash .*recover\.sh|bash -c .*multihop|bash -c .*recover" 2>/dev/null | grep -vw $$ || true)
    [ -n "$pids" ] && { echo "killing harness shells: $pids"; kill $pids 2>/dev/null; sleep 2; kill -9 $pids 2>/dev/null; }
    pids=$(pgrep -f "lampod-cli --data-dir $SIMDIR/h[smr] " 2>/dev/null || true)
    [ -n "$pids" ] && { echo "killing mh nodes: $pids"; kill -9 $pids 2>/dev/null; sleep 2; }
    pgrep -f "bash .*multihop\.sh|bash .*recover\.sh|lampod-cli --data-dir $SIMDIR/h[smr] " >/dev/null 2>&1 || { echo "all stopped"; return 0; }
    sleep 3
  done
  echo "could not stop everything:"; pgrep -af "multihop|recover|sim-run/h[smr]" | grep -v pgrep | head -5
  return 1
}

case $CMD in
  stop)   stop_all ;;
  soak)
    stop_all || exit 2
    rm -rf "$SIMDIR"/hs "$SIMDIR"/hm "$SIMDIR"/hr "$SIMDIR"/mh "$SIMDIR"/rc
    cd "$HARNESS" && setsid nohup bash -c "MH_ROUNDS=0 SEED=1337 ./multihop.sh" \
        > "$SIMDIR/mh-soak.log" 2>&1 < /dev/null &
    echo "endless multihop soak launched" ;;
  recover)
    stop_all || exit 2
    rm -rf "$SIMDIR"/rc
    cd "$HARNESS" && setsid nohup bash -c "STRESS_CYCLES=8 SEED=1337 ./recover.sh" \
        > "$SIMDIR/rc-boot.log" 2>&1 < /dev/null &
    echo "recovery matrix+stress launched" ;;
  *) echo "unknown cmd"; exit 1 ;;
esac
