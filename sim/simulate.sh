#!/usr/bin/env bash
#
# lampo pre-production soak simulation.
#
# SimLN-style defined/random payment activity + chaos events on a private
# regtest cluster, with hard assertions and artifact collection on failure.
# See docs/simulation/2026-08-14-simulation-plan.md for the full plan.
#
# Reuses hard-won lessons from ~/multihop.sh:
#   - never delete lampod.pid (flock on unlinked path = two daemons = corrupt `manager`)
#   - wait for funding tx in mempool BEFORE mining (confirmations racing the tx)
#   - assert payment state=="Success" AND preimage (never grep the response text)
#   - wait 150s after channels-ready before BOLT12 offers (announcer ticks 60s)
#   - cold data dirs sometimes need a second launch
#
# Config via env vars (all optional): NODES ROUNDS SEED PAY_MIN_MSAT
# PAY_MAX_MSAT CHAOS_EVERY METHODS TMO KEEP_GOING BIN CORE_URL CORE_USER
# CORE_PASS API_BASE P2P_BASE
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
CHAOS_EVENTS=(restart9 storm reorg feespam churn zapconn)
run_chaos() {
  local ev; ev=$(rand_pick "chaos-$1" "${CHAOS_EVENTS[@]}")
  "chaos_$ev" "$1-$ev"
  health_scan || fail "health scan tripped after chaos '$ev'"
}

# --- payments --------------------------------------------------------
# pay_probe <src>: small known-good payment to a random OTHER node (asserted)
pay_probe() {
  local src=$1 dst amt inv offer res state pre
  dst=$(rand_pick "probe-$1-dst" "${NAMES[@]}"); [ "$dst" = "$src" ] && dst=$([ "$src" = "${NAMES[0]}" ] && echo "${NAMES[1]}" || echo "${NAMES[0]}")
  amt=1000000
  inv=$(TMO=30 rpc "$(API "${dst#n}")" invoice "{\"amount_msat\":$amt,\"description\":\"probe\"}" | jqf 'd.get("bolt11","")')
  [ -n "$inv" ] || { say "probe: $dst issued no invoice"; return 1; }
  res=$(TMO=90 rpc "$(API "${src#n}")" pay "{\"invoice_str\":\"$inv\"}")
  state=$(echo "$res" | jqf 'd.get("state","")'); pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  [ "$state" = "Success" ] && [ -n "$pre" ]
}
# do_round: one simulated payment, method chosen from $METHODS
do_round() {
  local r=$1 src dst m amt t0 t1 res state pre dur ok=FAIL
  src=$(rand_pick "round-$r-src" "${NAMES[@]}")
  dst=$(rand_pick "round-$r-dst" "${NAMES[@]}")
  local tries=0
  while [ "$dst" = "$src" ] && [ "$tries" -lt 4 ]; do
    tries=$((tries+1)); dst=$(rand_pick "round-$r-dst-$tries" "${NAMES[@]}")
  done
  [ "$dst" = "$src" ] && dst=$([ "$src" = "${NAMES[0]}" ] && echo "${NAMES[1]}" || echo "${NAMES[0]}")
  m=$(rand_pick "round-$r-m" $METHODS)
  amt=$(rand_amount "round-$r")
  t0=$(date +%s)
  case $m in
    invoice)
      local inv; inv=$(TMO=30 rpc "$(API "${dst#n}")" invoice "{\"amount_msat\":$amt,\"description\":\"round $r\"}" | jqf 'd.get("bolt11","")')
      [ -n "$inv" ] || { say "round $r: $dst issued no invoice"; echo "$(date -Iseconds),$r,$src,$dst,$m,$amt,NoInvoice,,$(( $(date +%s)-t0 )), " >> "$CSV"; return 1; }
      res=$(TMO=120 rpc "$(API "${src#n}")" pay "{\"invoice_str\":\"$inv\"}")
      ;;
    offer)
      local off; off=$(TMO=30 rpc "$(API "${dst#n}")" offer "{\"amount_msat\":$amt,\"description\":\"round $r\"}" | jqf 'd.get("bolt12","")')
      [ -n "$off" ] || { say "round $r: $dst issued no offer"; echo "$(date -Iseconds),$r,$src,$dst,$m,$amt,NoOffer,,$(( $(date +%s)-t0 )), " >> "$CSV"; return 1; }
      res=$(TMO=120 rpc "$(API "${src#n}")" pay "{\"invoice_str\":\"$off\",\"amount\":$amt}")
      ;;
    keysend)
      # json_keysend returns {} on success; failures surface as RPC "error".
      res=$(TMO=120 rpc "$(API "${src#n}")" keysend "{\"destination\":\"${ID[$dst]}\",\"amount_msat\":$amt}")
      ;;
  esac
  t1=$(date +%s); dur=$((t1-t0))
  state=$(echo "$res" | jqf 'd.get("state","")')
  pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  if [ "$m" = keysend ]; then
    local err; err=$(echo "$res" | jqf 'd.get("error",{}).get("message","")')
    if [ -z "$err" ]; then state=Success; pre=keysend-ok; else state="RpcError:$err"; pre=""; fi
  fi
  if [ "$state" = "Success" ] && [ -n "$pre" ]; then ok=OK; else ok=FAIL; fi
  local hops=" "
  if [ "$m" != keysend ]; then hops=$(echo "$res" | jqf 'len(d.get("path",[]))'); fi
  echo "$(date -Iseconds),$r,$src,$dst,$m,$amt,$state,${pre:0:16},$dur,${hops:- }" >> "$CSV"
  if [ "$ok" = OK ]; then
    say "round $r OK: $src -> $dst via $m ${amt}msat (${dur}s, preimage ${pre:0:8}..)"
  else
    say "round $r FAIL: $src -> $dst via $m ${amt}msat state=${state:-none} dur=${dur}s"
    say "  raw: $(echo "$res" | head -c 300)"
    fail "round $r payment $src->$dst ($m) state=${state:-none}"
  fi
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
# fight over nodes, ports and this log)
if pgrep -f "bash.*$(basename "$0")" | grep -vq "^$$\$" && pgrep -f "[s]imulate.sh" | grep -vq "^$$\$"; then
  say "another simulate.sh instance is already running — refusing to start"
  exit 3
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

say "phase 6: activity loop (rounds=$ROUNDS, chaos every $CHAOS_EVERY)"
r=0
while :; do
  r=$((r+1))
  do_round "$r"
  if [ "$(( r % CHAOS_EVERY ))" = 0 ]; then run_chaos "$r"; fi
  health_scan || fail "periodic health scan"
  [ "$ROUNDS" != 0 ] && [ "$r" -ge "$ROUNDS" ] && break
done

say "SIMULATION COMPLETE: $r rounds, $(grep -c ',OK\|,Success' "$CSV" 2>/dev/null || echo 0) successful payments recorded in $CSV"
exit 0
