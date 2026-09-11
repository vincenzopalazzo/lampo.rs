#!/usr/bin/env bash
#
# lampo pre-production soak simulation.
#
# SimLN-style defined/random payment activity + chaos events on a private
# regtest cluster, with hard assertions and artifact collection on failure.
# See simulations/README.md for how to run Phase 1 / Phase 2 gates.
#
# Reuses hard-won lessons from earlier multihop soaks:
#   - never delete lampod.pid (flock on unlinked path = two daemons = corrupt `manager`)
#   - wait for funding tx in mempool BEFORE mining (confirmations racing the tx)
#   - assert payment state=="Success" AND preimage (never grep the response text)
#   - wait 150s after channels-ready before BOLT12 offers (announcer ticks 60s)
#   - cold data dirs sometimes need a second launch
#
# Config via env vars (all optional): NODES ROUNDS SEED PAY_MIN_MSAT
# PAY_MAX_MSAT CHAOS_EVERY METHODS TMO KEEP_GOING BIN CORE_URL CORE_USER
# CORE_PASS API_BASE P2P_BASE ROLE_MATRIX MIN_MULTIHOP
#
# Phase 2 proves lampo as sender AND receiver (edge-role matrix + CSV
# coverage gate). SimLN is optional relay load only — see simln/README.md.

set -uo pipefail

REPO=${REPO:-$HOME/lampo-sim}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$REPO/sim-run}
Nnodes=${NODES:-6}
ROUNDS=${ROUNDS:-10}
SEED=${SEED:-42}
PAY_MIN_MSAT=${PAY_MIN_MSAT:-10000}
PAY_MAX_MSAT=${PAY_MAX_MSAT:-50000000}
CHAOS_EVERY=${CHAOS_EVERY:-5}
METHODS=${METHODS:-"invoice offer keysend"}
ROLE_MATRIX=${ROLE_MATRIX:-1}
MIN_MULTIHOP=${MIN_MULTIHOP:-1}
TMO=${TMO:-60}
KEEP_GOING=${KEEP_GOING:-0}
API_BASE=${API_BASE:-8100}
P2P_BASE=${P2P_BASE:-19900}
CORE_URL=${CORE_URL:-http://127.0.0.1:18332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
ART=$SIMDIR/artifacts
CSV=$SIMDIR/results.csv
LOG=$SIMDIR/sim.log

# Seeded RNG: every draw is `random.Random("<seed>:<tag>)` — stateless,
# deterministic, reproducible. (Two earlier attempts failed: a hand-rolled
# xorshift in bash's signed 64-bit arithmetic collapsed into a 3-value
# cycle; a pool+index lost its index inside $() subshells.)
rand0() { # rand0 <tag> <n>  -> deterministic value in [0,n)
  python3 -c "import random;print(random.Random('$SEED:$1').randrange($2))"
}
rand_pick() { # rand_pick <tag> <item...>
  local tag=$1; shift
  local i=$(( $(rand0 "$tag" $#) + 1 ))
  echo "${!i}"
}
# log-uniform amount in [min,max] msat, per-tag deterministic
rand_amount() { # rand_amount <tag>
  python3 -c "
import math, random
r = random.Random('$SEED:amt:$1')
print(int(math.exp(math.log($PAY_MIN_MSAT) + (math.log($PAY_MAX_MSAT)-math.log($PAY_MIN_MSAT)) * r.random())))"
}

bcli() { # bcli <method> [params-json] -> result json (uses the loaded `default` wallet)
  curl -sS --max-time 30 --user "$CORE_USER:$CORE_PASS" \
    --data-binary "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" \
    "$CORE_URL/wallet/default"
}
bcres() { bcli "$@" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(json.dumps(d.get("result") if "result" in d else d.get("error")))' 2>/dev/null; }
mine() { local a; a=$(bcres getnewaddress | tr -d '"'); bcli generatetoaddress "[${1:-6},\"$a\"]" >/dev/null 2>&1; }
rpc() { curl -sS --max-time "$TMO" -X POST "http://127.0.0.1:$1/$2" -H 'content-type: application/json' -d "${3:-{\}}"; }
jqf() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
say() { echo "[$(date +%m-%d\ %H:%M:%S)] $*" | tee -a "$LOG"; }

NAMES=()
for i in $(seq 1 "$Nnodes"); do NAMES+=("n$i"); done
API() { echo $((API_BASE + $1)); }
P2P() { echo $((P2P_BASE + $1)); }

collect_artifacts() { # $1 = tag
  local dir="$ART/$(date +%Y%m%d-%H%M%S)-$1"
  mkdir -p "$dir"
  for n in "${NAMES[@]}"; do
    cp -r "$SIMDIR/$n" "$dir/" 2>/dev/null
    tail -c 200000 "$SIMDIR/$n/mh.log" > "$dir/$n-tail.log" 2>/dev/null
  done
  cp "$CSV" "$LOG" "$dir/" 2>/dev/null
  bcres getmempoolinfo > "$dir/mempool.json" 2>/dev/null
  for n in "${NAMES[@]}"; do
    rpc "$(API "${n#n}")" getinfo > "$dir/$n-getinfo.json" 2>/dev/null
  done
  say "artifacts collected in $dir"
}

fail() { say "FAIL: $*"; collect_artifacts "$(echo "$*" | tr ' /' '__' | head -c 40)"; [ "$KEEP_GOING" = 1 ] || exit 2; }

start_node() {
  local n=$1 idx=${1#n} dir="$SIMDIR/$n"
  mkdir -p "$dir/regtest"
  cat > "$dir/regtest/lampo.conf" <<EOF
network=regtest
port=$(P2P "$idx")
announce-addr=127.0.0.1
api-host=http://127.0.0.1
api-port=$(API "$idx")
backend=core
core-url=$CORE_URL
core-user=$CORE_USER
core-pass=$CORE_PASS
EOF
  # Do NOT remove lampod.pid (see header comment).
  setsid nohup "$BIN" --data-dir "$dir" --network regtest \
      > "$dir/mh.log" 2>&1 < /dev/null &
  disown 2>/dev/null || true
}

wait_up() { # $1 name -> echoes node_id
  local n=$1 idx=${1#n}
  for _ in $(seq 1 3); do
    for _ in $(seq 1 12); do
      sleep 5
      local id; id=$(rpc "$(API "$idx")" getinfo | jqf 'd["node_id"]')
      [ -n "$id" ] && { echo "$id"; return 0; }
    done
    start_node "$n"   # cold dir sometimes needs a second launch
  done
  return 1
}

node_pid() { pgrep -f "lampod-cli --data-dir $SIMDIR/$1 " | head -1; }

fund_node() { # $1 name $2 btc
  local idx=${1#n} addr
  addr=$(rpc "$(API "$idx")" new_addr | jqf 'd["address"]')
  [ -n "$addr" ] || { say "no address from $1"; return 1; }
  bcres sendtoaddress "[\"$addr\", $2]" >/dev/null
  mine 6
}

open_channel() { # from-name to-name to-id [amount]
  local from=$1 to=$2 id=$3 amt=${4:-1000000}
  rpc "$(API "${from#n}")" connect "{\"node_id\":\"$id\",\"addr\":\"127.0.0.1\",\"port\":$(P2P "${to#n}")}" >/dev/null
  # Capture the fundchannel response: discarding it (like the first smoke
  # run did) turns a failing/hanging open into a silent empty channel list.
  local resp
  resp=$(TMO=150 rpc "$(API "${from#n}")" fundchannel \
    "{\"node_id\":\"$id\",\"addr\":\"127.0.0.1\",\"port\":$(P2P "${to#n}"),\"amount\":$amt,\"public\":true}")
  # A 4xx from actix returns a plain-text body ("Json deserialize error: ...")
  # which is not valid JSON: treat that as a failure too, not just {"error":...}
  case "$resp" in
    "{"*) : ;;
    *) say "open_channel $from->$to non-JSON response: $(echo "$resp" | head -c 200)"; return 1 ;;
  esac
  if echo "$resp" | jqf 'd.get("error",{}).get("message","")' | grep -q .; then
    say "open_channel $from->$to RPC error: $(echo "$resp" | head -c 300)"
    return 1
  fi
  # wait funding tx hits mempool BEFORE mining (race lesson)
  local sz=0
  for _ in $(seq 1 20); do
    sz=$(bcli getmempoolinfo | jqf 'd["result"]["size"]'); [ "${sz:-0}" -gt 0 ] 2>/dev/null && break; sleep 3
  done
  mine 8
}

ready_channels() { rpc "$(API "${1#n}")" channels | jqf 'sum(1 for c in d["channels"] if c["ready"])'; }
peers_of() { rpc "$(API "${1#n}")" getinfo | jqf 'd["peers"]'; }

# wait until every node's wallet catches up with the chain tip (the wallet
# applies ~1 block/s in 2-min windows, so big mined stretches lag badly —
# the first smoke run failed exactly here: fundchannel hit a wallet whose
# funding UTXOs were not spendable yet)
wait_wallet_synced() { # [timeout_s]
  local deadline=$(( $(date +%s) + ${1:-420} ))
  for n in "${NAMES[@]}"; do
    local idx=${n#n} info h w
    while :; do
      info=$(rpc "$(API "$idx")" getinfo)
      h=$(echo "$info" | jqf 'd.get("blockheight",-1)'); w=$(echo "$info" | jqf 'd.get("wallet_height",-2)')
      [ "$(( h - w ))" -le 1 ] && break
      [ "$(date +%s)" -gt "$deadline" ] && { say "wallet of $n still behind: chain=$h wallet=$w"; return 1; }
      sleep 10
    done
  done
  say "all wallets synced to chain tip"
}

# --- health monitor: panics / errors in node logs -------------------
health_scan() {
  local hits
  hits=$(grep -hniE "panic|corrupt|invariant" "$SIMDIR"/*/mh.log 2>/dev/null | grep -v "grep" | head -5)
  [ -n "$hits" ] && { say "HEALTH: suspicious log lines:"; echo "$hits" | tee -a "$LOG"; return 1; }
  return 0
}

# --- chaos events ----------------------------------------------------
chaos_restart9() { # $1 = tag
  local n; n=$(rand_pick "chaos-$1-n" "${NAMES[@]:1}")   # never n1: it funds the probe payments
  say "CHAOS restart9: SIGKILL $n (channels open)"
  local p; p=$(node_pid "$n")
  [ -n "$p" ] && kill -9 "$p"
  sleep 2
  start_node "$n"
  wait_up "$n" >/dev/null || { fail "restart9: $n never came back"; return; }
  sleep 10
  pay_probe "n1" || fail "restart9: payment probe failed after $n restart"
}
# Graceful SIGTERM must release lampod.pid (see PR #603). Escalate to
# SIGKILL only if the process ignores TERM past the deadline — same as recover.
chaos_restart_term() { # $1 = tag
  local n deadline p
  n=$(rand_pick "chaos-$1-n" "${NAMES[@]:1}")
  say "CHAOS restart_term: SIGTERM $n (channels open)"
  p=$(node_pid "$n")
  [ -n "$p" ] && kill -TERM "$p"
  deadline=$(( $(date +%s) + 60 ))
  while [ -n "$(node_pid "$n")" ] && [ "$(date +%s)" -le "$deadline" ]; do sleep 2; done
  if [ -n "$(node_pid "$n")" ]; then
    say "CHAOS restart_term: $n still alive after 60s — escalating SIGKILL"
    kill -9 "$(node_pid "$n")" 2>/dev/null; sleep 2
  fi
  start_node "$n"
  wait_up "$n" >/dev/null || { fail "restart_term: $n never came back"; return; }
  sleep 10
  pay_probe "n1" || fail "restart_term: payment probe failed after $n restart"
}
chaos_storm() { # $1 = tag
  local k=$(( 10 + $(rand0 "storm-$1" 40) ))
  say "CHAOS storm: mining $k blocks at once"
  mine "$k"
}
chaos_reorg() {
  say "CHAOS reorg: invalidating tip and forking"
  local tip; tip=$(bcres getbestblockhash | tr -d '"')
  bcli invalidateblock "[\"$tip\"]" >/dev/null 2>&1
  mine 3
  # Tip invalidate leaves gossip SCIDs briefly stale; paying through a
  # half-rebuilt graph yields transient ChannelFailure → RetriesExhausted
  # (seed=99 round 7 BOLT12). Wallet sync alone is not enough: LDK still
  # returns TemporaryChannelFailure on mid-hops for ~1–2 min while channel
  # monitors / gossip catch the new tip. Deepen the fork, wait, then probe
  # until a real payment succeeds before resuming the round loop.
  wait_wallet_synced 180 || true
  mine 6
  wait_wallet_synced 180 || true
  sleep 60
  local i=0
  while [ "$i" -lt 8 ]; do
    if pay_probe "n1"; then
      say "CHAOS reorg: post-reorg probe OK (attempt $((i+1)))"
      return 0
    fi
    i=$((i+1))
    say "CHAOS reorg: probe failed (attempt $i/8) — mining+waiting"
    mine 2
    sleep 20
  done
  fail "reorg: payment probe never recovered after tip invalidate"
}
chaos_feespam() {
  say "CHAOS feespam: 50 txs into mempool + estimate"
  for i in $(seq 1 50); do
    local a; a=$(bcres getnewaddress | tr -d '"')
    bcres sendtoaddress "[\"$a\", 0.001]" >/dev/null 2>&1
  done
  local feerate; feerate=$(bcli estimatesmartfee "[2,\"CONSERVATIVE\"]" | jqf 'd["result"]["feerate"]')
  say "CHAOS feespam: est feerate(2)=$feerate"
}
chaos_churn() { # $1 = tag
  local n; n=$(rand_pick "churn-$1-n" "${NAMES[@]:1}")
  local cid pid
  pid=$(rpc "$(API "${n#n}")" channels | jqf '(d["channels"][0]["peer_id"] if d.get("channels") else "")')
  [ -z "$pid" ] && { say "CHAOS churn: no channel found, skip"; return; }
  say "CHAOS churn: closing a channel of $n (peer ${pid:0:12}..) and reopening"
  rpc "$(API "${n#n}")" close "{\"node_id\":\"$pid\"}" >/dev/null 2>&1
  sleep 20; mine 6; sleep 20
  open_channel "$n" "$(churn_peer_name "$pid")" "$pid" 1000000 || say "CHAOS churn: reopen via helper failed (may reconnect async)"
}
churn_peer_name() { # map peer_id back to a node name via stored IDs
  for n in "${NAMES[@]}"; do [ "${ID[$n]:-}" = "$1" ] && echo "$n" && return; done
  echo "${NAMES[0]}"
}
chaos_zapconn() { # $1 = tag
  local n; n=$(rand_pick "zap-$1-n" "${NAMES[@]:1}") idx=${n#n}
  say "CHAOS zapconn: killing TCP conns of $n (auto-reconnect regression)"
  ss -K state established "( sport = :$(P2P "$idx") or dport = :$(P2P "$idx") )" 2>/dev/null | head -2 >/dev/null
  sleep 15
  pay_probe "n1" || fail "zapconn: payment probe failed after connection loss on $n"
}
CHAOS_EVENTS=(restart9 restart_term storm reorg feespam churn zapconn)
run_chaos() {
  local ev; ev=$(rand_pick "chaos-$1" "${CHAOS_EVENTS[@]}")
  "chaos_$ev" "$1-$ev"
  # Give peers a moment to reconnect / refresh channel monitors before
  # the next payment round hammers a still-settling graph.
  sleep 10
  health_scan || fail "health scan tripped after chaos '$ev'"
}

# --- payments --------------------------------------------------------
# do_pay <tag> <src> <dst> <method> <amt> [min_hops]
# Core payment primitive used by probes, the edge-role matrix, and soak
# rounds. Asserts Success+preimage; when min_hops is set, also requires
# len(path) >= min_hops so lampo is proven as a multi-hop endpoint (not
# only a direct peer payment).
do_pay() {
  local tag=$1 src=$2 dst=$3 m=$4 amt=$5 min_hops=${6:-0}
  local t0 t1 res state pre dur ok=FAIL hops=" " attempt max_attempts
  t0=$(date +%s)
  case $m in
    invoice)
      local inv
      inv=$(TMO=30 rpc "$(API "${dst#n}")" invoice "{\"amount_msat\":$amt,\"description\":\"$tag\"}" | jqf 'd.get("bolt11","")')
      [ -n "$inv" ] || { say "$tag: $dst issued no invoice"; echo "$(date -Iseconds),$tag,$src,$dst,$m,$amt,NoInvoice,,$(( $(date +%s)-t0 )), " >> "$CSV"; return 1; }
      res=$(TMO=120 rpc "$(API "${src#n}")" pay "{\"invoice_str\":\"$inv\"}")
      ;;
    offer)
      local off
      off=$(TMO=30 rpc "$(API "${dst#n}")" offer "{\"amount_msat\":$amt,\"description\":\"$tag\"}" | jqf 'd.get("bolt12","")')
      [ -n "$off" ] || { say "$tag: $dst issued no offer"; echo "$(date -Iseconds),$tag,$src,$dst,$m,$amt,NoOffer,,$(( $(date +%s)-t0 )), " >> "$CSV"; return 1; }
      res=$(TMO=120 rpc "$(API "${src#n}")" pay "{\"invoice_str\":\"$off\",\"amount\":$amt}")
      ;;
    keysend)
      res=$(TMO=120 rpc "$(API "${src#n}")" keysend "{\"destination\":\"${ID[$dst]}\",\"amount_msat\":$amt}")
      ;;
    *) say "$tag: unknown method $m"; return 1 ;;
  esac
  t1=$(date +%s); dur=$((t1-t0))
  state=$(echo "$res" | jqf 'd.get("state","")')
  pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  if [ "$m" != keysend ]; then hops=$(echo "$res" | jqf 'len(d.get("path",[]))'); fi
  if [ "$state" = "Success" ] && [ -n "$pre" ]; then ok=OK; fi
  if [ "$ok" = OK ] && [ "$min_hops" -gt 0 ]; then
    if [ "$m" = keysend ] || [ "${hops:-0}" -lt "$min_hops" ]; then
      ok=FAIL
      say "$tag: hop assert failed (method=$m hops=${hops:-?} want>=$min_hops)"
    fi
  fi
  echo "$(date -Iseconds),$tag,$src,$dst,$m,$amt,$state,${pre:0:16},$dur,${hops:- }" >> "$CSV"
  if [ "$ok" = OK ]; then
    say "$tag OK: $src -> $dst via $m ${amt}msat (${dur}s, hops=${hops:-?}, preimage ${pre:0:8}..)"
    return 0
  fi
  # Transient TemporaryChannelFailure → RetriesExhausted is common right
  # after chain chaos (reorg/storm). Retry the same src/dst/method a few
  # times before declaring the payment dead so we don't fail the soak on
  # gossip lag (seed=99 round 7 BOLT12 after reorg).
  attempt=1; max_attempts=3
  while [ "$attempt" -lt "$max_attempts" ]; do
    attempt=$((attempt+1))
    say "$tag: transient FAIL state=${state:-none} — retry $attempt/$max_attempts in 20s"
    sleep 20
    case $m in
      invoice)
        local inv2
        inv2=$(TMO=30 rpc "$(API "${dst#n}")" invoice "{\"amount_msat\":$amt,\"description\":\"$tag retry$attempt\"}" | jqf 'd.get("bolt11","")')
        [ -n "$inv2" ] || continue
        res=$(TMO=120 rpc "$(API "${src#n}")" pay "{\"invoice_str\":\"$inv2\"}")
        ;;
      offer)
        local off2
        off2=$(TMO=30 rpc "$(API "${dst#n}")" offer "{\"amount_msat\":$amt,\"description\":\"$tag retry$attempt\"}" | jqf 'd.get("bolt12","")')
        [ -n "$off2" ] || continue
        res=$(TMO=120 rpc "$(API "${src#n}")" pay "{\"invoice_str\":\"$off2\",\"amount\":$amt}")
        ;;
      keysend)
        res=$(TMO=120 rpc "$(API "${src#n}")" keysend "{\"destination\":\"${ID[$dst]}\",\"amount_msat\":$amt}")
        ;;
    esac
    state=$(echo "$res" | jqf 'd.get("state","")')
    pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
    hops=" "
    if [ "$m" != keysend ]; then hops=$(echo "$res" | jqf 'len(d.get("path",[]))'); fi
    ok=FAIL
    if [ "$state" = "Success" ] && [ -n "$pre" ]; then ok=OK; fi
    if [ "$ok" = OK ] && [ "$min_hops" -gt 0 ]; then
      if [ "$m" = keysend ] || [ "${hops:-0}" -lt "$min_hops" ]; then ok=FAIL; fi
    fi
    dur=$(( $(date +%s) - t0 ))
    echo "$(date -Iseconds),$tag,$src,$dst,$m,$amt,$state,${pre:0:16},$dur,${hops:- }" >> "$CSV"
    if [ "$ok" = OK ]; then
      say "$tag OK (retry $attempt): $src -> $dst via $m ${amt}msat (${dur}s, hops=${hops:-?})"
      health_scan || fail "health scan tripped after $tag"
      return 0
    fi
  done
  say "$tag FAIL: $src -> $dst via $m ${amt}msat state=${state:-none} dur=${dur}s hops=${hops:-?}"
  say "  raw: $(echo "$res" | head -c 300)"
  return 1
}

# pay_probe <src>: small known-good payment to a random OTHER node (asserted)
pay_probe() {
  local src=$1 dst
  dst=$(rand_pick "probe-$1-dst" "${NAMES[@]}")
  [ "$dst" = "$src" ] && dst=$([ "$src" = "${NAMES[0]}" ] && echo "${NAMES[1]}" || echo "${NAMES[0]}")
  do_pay "probe-$src" "$src" "$dst" invoice 1000000
}

# opposite_node <name>: diametric peer on the ring (forces multi-hop when N>=3)
# peer_other <name>: a different node (prefer ring-opposite for path diversity)
peer_other() {
  local n=$1 i idx=0 opp
  for i in "${!NAMES[@]}"; do
    if [ "${NAMES[$i]}" = "$n" ]; then idx=$i; break; fi
  done
  opp=$(( (idx + Nnodes / 2) % Nnodes ))
  if [ "${NAMES[$opp]}" = "$n" ]; then
    echo "$([ "$n" = "${NAMES[0]}" ] && echo "${NAMES[1]}" || echo "${NAMES[0]}")"
  else
    echo "${NAMES[$opp]}"
  fi
}

# is_direct_chord <a> <b>: true if simulate.sh opens a chord between them
is_direct_chord() {
  case "$1:$2" in
    n1:n3|n3:n1) [ "$Nnodes" -ge 3 ] && return 0 ;;
    n4:n6|n6:n4) [ "$Nnodes" -ge 6 ] && return 0 ;;
  esac
  return 1
}

# find_multihop_pair: first ring pair with distance >=2 and no chord, so a
# Success payment must actually route (not a direct channel). Empty if none.
find_multihop_pair() {
  local i j d a b d2
  for i in "${!NAMES[@]}"; do
    for j in "${!NAMES[@]}"; do
      [ "$i" = "$j" ] && continue
      d=$(( (j - i + Nnodes) % Nnodes ))
      d2=$(( Nnodes - d ))
      [ "$d2" -lt "$d" ] && d=$d2
      [ "$d" -lt 2 ] && continue
      a=${NAMES[$i]}; b=${NAMES[$j]}
      is_direct_chord "$a" "$b" && continue
      echo "$a $b"
      return 0
    done
  done
  return 1
}

# run_edge_matrix: prove every lampo node can SEND and RECEIVE, across
# invoice/offer/keysend, plus a multi-hop subset when the topology allows.
# SimLN can only drive LDK edges; this matrix is the Phase 2 send/recv proof.
run_edge_matrix() {
  local n other m amt done=0 src dst pair
  say "phase 5.5: edge-role matrix (every node sends + receives)"
  for n in "${NAMES[@]}"; do
    other=$(peer_other "$n")
    amt=$(rand_amount "edge-send-$n")
    do_pay "edge-send-$n" "$n" "$other" invoice "$amt" || fail "edge-send $n->$other invoice"
  done
  for n in "${NAMES[@]}"; do
    other=$(peer_other "$n")
    amt=$(rand_amount "edge-recv-$n")
    # other pays n → n is the receiver under test
    do_pay "edge-recv-$n" "$other" "$n" invoice "$amt" || fail "edge-recv $other->$n invoice"
  done
  # Method coverage with lampo on both ends (not just invoice).
  for m in $METHODS; do
    [ "$m" = invoice ] && continue
    amt=$(rand_amount "edge-method-$m")
    do_pay "edge-method-$m" "${NAMES[0]}" "${NAMES[1]}" "$m" "$amt"       || fail "edge-method $m ${NAMES[0]}->${NAMES[1]}"
  done
  # Structural multi-hop when a non-adjacent, non-chord pair exists.
  # Tiny rings (N<4) are fully adjacent — hop proof lives in multihop.sh.
  if [ "$MIN_MULTIHOP" -gt 0 ]; then
    done=0
    if pair=$(find_multihop_pair); then
      src=${pair%% *}; dst=${pair##* }
      amt=$(rand_amount "edge-mh-0")
      if do_pay "edge-mh-0" "$src" "$dst" invoice "$amt" 2; then
        done=1
      else
        fail "edge-multihop $src->$dst (need hops>=2)"
      fi
    fi
    if [ "$done" -lt "$MIN_MULTIHOP" ]; then
      if [ "$Nnodes" -lt 4 ]; then
        say "edge-multihop: skipped (N=$Nnodes fully adjacent; use simulations/multihop.sh)"
      else
        fail "edge-multihop: no non-adjacent pair / only $done successes, want >=$MIN_MULTIHOP"
      fi
    fi
  fi
  health_scan || fail "health scan after edge-role matrix"
}

# assert_edge_coverage: Phase 2 gate — Success rows must cover every node
# as sender AND as receiver. Random soak alone can miss a node.
assert_edge_coverage() {
  python3 - "$CSV" "${NAMES[@]}" <<'PY'
import csv, sys
path, names = sys.argv[1], sys.argv[2:]
send = {n: 0 for n in names}
recv = {n: 0 for n in names}
mh = 0
with open(path) as f:
    r = csv.DictReader(f)
    for row in r:
        st = row.get("state", "")
        if st not in ("Success", "OK"):
            continue
        s, d = row.get("src", ""), row.get("dst", "")
        if s in send:
            send[s] += 1
        if d in recv:
            recv[d] += 1
        try:
            hops = int(str(row.get("hops", "")).strip() or 0)
        except ValueError:
            hops = 0
        tag = row.get("round", "")
        if hops >= 2 or str(tag).startswith("edge-mh-"):
            mh += 1
missing_s = [n for n, c in send.items() if c < 1]
missing_r = [n for n, c in recv.items() if c < 1]
print(f"coverage send={send} recv={recv} multihop_rows={mh}")
if missing_s:
    print(f"MISSING_SEND:{','.join(missing_s)}")
    sys.exit(2)
if missing_r:
    print(f"MISSING_RECV:{','.join(missing_r)}")
    sys.exit(3)
PY
}

# do_round: one simulated payment, method chosen from $METHODS
do_round() {
  local r=$1 src dst m amt
  src=$(rand_pick "round-$r-src" "${NAMES[@]}")
  dst=$(rand_pick "round-$r-dst" "${NAMES[@]}")
  local tries=0
  while [ "$dst" = "$src" ] && [ "$tries" -lt 4 ]; do
    tries=$((tries+1)); dst=$(rand_pick "round-$r-dst-$tries" "${NAMES[@]}")
  done
  [ "$dst" = "$src" ] && dst=$([ "$src" = "${NAMES[0]}" ] && echo "${NAMES[1]}" || echo "${NAMES[0]}")
  m=$(rand_pick "round-$r-m" $METHODS)
  amt=$(rand_amount "round-$r")
  do_pay "round-$r" "$src" "$dst" "$m" "$amt" || fail "round $r payment $src->$dst ($m)"
  health_scan || fail "health scan tripped after round $r"
}


# ============================ main ====================================
mkdir -p "$SIMDIR" "$ART"
: > "$CSV"; : > "$LOG"
echo "ts,round,src,dst,method,amount_msat,state,preimage16,dur_s,hops" > "$CSV"

say "phase 0: preflight (bin=$BIN nodes=$Nnodes rounds=$ROUNDS seed=$SEED)"
[ -x "$BIN" ] || { say "binary missing: $BIN"; exit 1; }
bcli getblockchaininfo | jqf 'd["result"]["chain"]' | grep -q regtest || { say "bitcoind at $CORE_URL not regtest"; exit 1; }
# kill leftovers from a previous run: they still hold the API/P2P ports
local_pids=$(pgrep -f "lampod-cli --data-dir $SIMDIR/" || true)
if [ -n "$local_pids" ]; then
  say "phase 0: killing leftover sim nodes: $local_pids"
  kill -9 $local_pids 2>/dev/null || true
  sleep 3
fi
# refuse to run twice against the same SIMDIR (a zombie harness would
# fight over nodes, ports and this log); other tiers on other SIMDIRs
# are fine (checked via /proc/<pid>/environ, same-user readable)
_others=$(pgrep -af "[s]imulate.sh" | grep -v "^$$" || true)
if [ -n "$_others" ]; then
  for _pid in $(echo "$_others" | awk '{print $1}'); do
    [ -r "/proc/$_pid/environ" ] || continue
    if tr '\0' '\n' < "/proc/$_pid/environ" | grep -q "^SIMDIR=$SIMDIR$"; then
      say "another simulate.sh instance against SIMDIR=$SIMDIR is already running — refusing to start"
      exit 1
    fi
  done
fi

say "phase 1: starting ${NAMES[*]}"
declare -A ID
for n in "${NAMES[@]}"; do
  start_node "$n"
  ID[$n]=$(wait_up "$n") || { say "node $n never came up"; exit 1; }
  say "  $n = ${ID[$n]:0:16}… (api :$(API "${n#n}") p2p :$(P2P "${n#n}"))"
done

say "phase 2: funding ${NAMES[*]}"
for n in "${NAMES[@]}"; do fund_node "$n" 0.05 || fail "funding $n"; done
say "waiting wallet sync (production sync schedule: every 2 min)"
sleep 140
wait_wallet_synced 420 || fail "wallets never synced after funding"

say "phase 3: opening channels (ring + chords)"
# ring n_i -> n_{i+1} (opener holds outbound)
for i in $(seq 1 $((Nnodes-1))); do open_channel "n$i" "n$((i+1))" "${ID[n$((i+1))]}"; done
open_channel "n$Nnodes" "n1" "${ID[n1]}"
# chords (only meaningful with >=4 nodes): n1->n3, n4->n6
if [ "$Nnodes" -ge 3 ]; then open_channel "n1" "n3" "${ID[n3]}"; fi
if [ "$Nnodes" -ge 6 ]; then open_channel "n4" "n6" "${ID[n6]}"; fi
# channels need 6 confs to become ready; give wallets time to process them
sleep 30
wait_wallet_synced 300 || true

say "phase 4: assertions (retry up to 180s: readiness lags confirmations)"
a_deadline=$(( $(date +%s) + 180 ))
while :; do
  a_ok=1
  for n in "${NAMES[@]}"; do
    p=$(peers_of "$n"); c=$(ready_channels "$n")
    say "  $n peers=$p ready_channels=$c"
    [ "${p:-0}" -ge 2 ] || a_ok=0
    [ "${c:-0}" -ge 1 ] || a_ok=0
  done
  [ "$a_ok" = 1 ] && break
  [ "$(date +%s)" -gt "$a_deadline" ] && { fail "assertions failed after 180s (see above)"; break; }
  sleep 15
done
say "phase 5: waiting 150s for node_announcement propagation (BOLT12 precondition)"
sleep 150


if [ "$ROLE_MATRIX" = 1 ]; then
  run_edge_matrix
else
  say "phase 5.5: edge-role matrix skipped (ROLE_MATRIX=$ROLE_MATRIX)"
fi

say "phase 6: activity loop (rounds=$ROUNDS, chaos every $CHAOS_EVERY)"
r=0
while :; do
  r=$((r+1))
  do_round "$r"
  if [ "$(( r % CHAOS_EVERY ))" = 0 ]; then run_chaos "$r"; fi
  health_scan || fail "periodic health scan"
  [ "$ROUNDS" != 0 ] && [ "$r" -ge "$ROUNDS" ] && break
done

cov_out=$(assert_edge_coverage) || { say "edge coverage FAILED: $cov_out"; fail "edge-role coverage gate"; }
say "edge coverage OK: $cov_out"
say "SIMULATION COMPLETE: $r rounds, $(grep -c ',Success' "$CSV" 2>/dev/null || echo 0) successful payments recorded in $CSV"
exit 0
