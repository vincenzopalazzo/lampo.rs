#!/usr/bin/env bash
#
# sim/multihop.sh — dedicated multi-hop payment harness.
#
# Why: the ring cluster of simulate.sh pays mostly over short routes and
# only LOGS the hop count; nothing forces a payment that cannot be direct.
# Here the topology is a dead-end chain, so multi-hop is a structural
# property, not a coincidence:
#
#     hs ──(ch1)── hm ──(ch2)── hr     hs and hr have NO channel between
#                                      them: every hs<->hr payment MUST
#                                      route hm, i.e. >= 2 hops.
#
# Ports API 8211-8213 / P2P 20111-20113, data $SIMDIR/mh/{hs,hm,hr}:
# disjoint from the endless simulate.sh soak (8101../19901..) — both can
# run at once on the debian server.
#
# Legacy note: the old ~/multihop.sh (node-s/m/r) last PASSED only against
# the OLD lampo-swapd-deploy binary; this harness targets the current
# sim/main build and asserts hop structure, not just "Success".
#
# Env: MH_ROUNDS(15, 0=endless) SEED PAY_MIN_MSAT(10000) PAY_MAX_MSAT(5000000)
#      MH_CHANNEL_AMT_SATS(1000000) ENSURE_ONLY(0) KEEP_GOING ...
# Results: $SIMDIR/mh/results.csv  Log: $SIMDIR/mh/sim.log
#
# Usage:
#   ./multihop.sh                 # build-or-reuse cluster, assert, stress
#   ENSURE_ONLY=1 ./multihop.sh   # just build/reuse the cluster (for recover.sh)
set -uo pipefail

API_BASE=${API_BASE:-8210}
P2P_BASE=${P2P_BASE:-20110}
SIMDIR=${SIMDIR:-$HOME/lampo-sim/sim-run}
MHDIR=${MHDIR:-$SIMDIR/mh}
LOG=${LOG:-$MHDIR/sim.log}
CSV=${CSV:-$MHDIR/results.csv}
MH_ROUNDS=${MH_ROUNDS:-15}
PAY_MIN_MSAT=${PAY_MIN_MSAT:-10000}
PAY_MAX_MSAT=${PAY_MAX_MSAT:-5000000}
MH_CHANNEL_AMT_SATS=${MH_CHANNEL_AMT_SATS:-1000000}
ENSURE_ONLY=${ENSURE_ONLY:-0}

source "$(dirname "$0")/lib.sh"

declare -A IDX=([hs]=1 [hm]=2 [hr]=3)
ALLNODES=(hs hm hr)
PAYER_NODES=(hs hr)          # hm is the relay; payers sit at the dead ends
declare -A ID=()

# ============================ main ====================================
mkdir -p "$MHDIR" "$ART"
: > "$LOG"
echo "ts,tag,src,dst,method,amount_msat,state,preimage16,dur_s,hops,relay_ok" > "$CSV"

say "multihop harness: bin=$BIN seed=$SEED rounds=$MH_ROUNDS ensure_only=$ENSURE_ONLY"
[ -x "$BIN" ] || { say "binary missing: $BIN"; exit 1; }
bcli getblockchaininfo | jqf 'd["result"]["chain"]' | grep -q regtest || { say "bitcoind at $CORE_URL not regtest"; exit 1; }
# duplicate-instance guard: pidfile-based (pgrep patterns race against
# our own pipeline forks, which briefly carry the parent's argv).
mkdir -p "$MHDIR"
GUARD_PIDFILE="$MHDIR/harness.pid"
if [ -f "$GUARD_PIDFILE" ]; then
  gp=$(cat "$GUARD_PIDFILE" 2>/dev/null)
  if [ -n "$gp" ] && [ -d "/proc/$gp" ] && grep -qa "multihop" "/proc/$gp/cmdline" 2>/dev/null; then
    say "another multihop.sh instance is already running (pid $gp) — refusing to start"
    exit 3
  fi
  say "stale harness pid $gp — taking over"
fi
echo $$ > "$GUARD_PIDFILE"
trap 'rm -f "$GUARD_PIDFILE"' EXIT
for p in $(API hs) $(API hm) $(API hr); do
  ss -ltn 2>/dev/null | grep -q ":$p " && { say "port :$p busy — mh cluster already running? (use the running one)"; exit 3; }
done

ensure_cluster || { fail "mh cluster not established"; exit 2; }
[ "$ENSURE_ONLY" = 1 ] && { say "ENSURE_ONLY: cluster ready, stopping here"; exit 0; }

say "phase 2: fixed multi-hop assertions (bolt11 both directions, offer, keysend)"
mh_pay a1-invoice-fwd  hs hr 1000000 invoice  || fail "a1 bolt11 hs->hr multihop"
mh_pay a2-invoice-rev  hr hs 1000000 invoice  || fail "a2 bolt11 hr->hs multihop"
mh_pay a3-offer-fwd    hs hr  400000 offer    || fail "a3 bolt12 offer hs->hr multihop"
mh_pay a4-keysend-fwd  hs hr  400000 keysend  || fail "a4 keysend hs->hr multihop"

say "phase 3: seeded stress rounds ($MH_ROUNDS, 0=endless)"
r=0
while :; do
  r=$((r+1))
  src=$(rand_pick "mh-$r-src" "${PAYER_NODES[@]}")
  dst=hs; [ "$src" = hs ] && dst=hr
  m=$(rand_pick "mh-$r-m" invoice offer keysend)
  amt=$(rand_amount "mh-$r" "$PAY_MIN_MSAT" "$PAY_MAX_MSAT")
  local_marks=$(log_marks)
  mh_pay "mh-$r" "$src" "$dst" "$amt" "$m" || fail "stress round $r $src->$dst ($m)"
  health_scan_since "$local_marks" || fail "health scan after mh round $r"
  [ "$MH_ROUNDS" != 0 ] && [ "$r" -ge "$MH_ROUNDS" ] && break
done

say "phase 4: relay bounce (light bridge to sim/recover.sh — the heavy matrix lives there)"
bounce_marks=$(log_marks)
kill9 hm; sleep 2
start_node hm
rid=$(wait_up hm) || fail "relay hm never came back"
[ "$rid" = "${ID[hm]}" ] || fail "relay hm came back with a DIFFERENT node_id"
sleep 10
mh_pay bounce hs hr 1000000 invoice || fail "post-bounce payment hs->hr"
health_scan_since "$bounce_marks" || fail "health scan after relay bounce"

say "MULTIHOP COMPLETE: $r stress rounds OK, results in $CSV"
exit 0
