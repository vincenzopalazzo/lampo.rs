#!/usr/bin/env bash
#
# sim/lib.sh — shared helpers for the lampo simulation harnesses.
#
# Sourced by multihop.sh / recover.sh. simulate.sh intentionally stays
# standalone: the endless soak on the server keeps running against the
# exact scripts it started with (never change code under a running soak).
#
# Env vars (set BEFORE sourcing; all optional):
#   REPO BIN SIMDIR ART LOG SEED TMO KEEP_GOING
#   API_BASE P2P_BASE CORE_URL CORE_USER CORE_PASS
#
# Node addressing: the sourcing script declares, at top level,
#   declare -A IDX=([hs]=1 [hm]=2 [hr]=3); ALLNODES=(hs hm hr)
# `API hs` -> API_BASE+1. Plain `n<k>` names keep the simulate.sh
# convention (IDX falls back to the numeric suffix).
#
# Lessons (paid for in blood on the debian server, see simulate.sh header):
#   - never delete lampod.pid (flock on an unlinked path = two daemons on
#     one `manager` file = corruption)
#   - wait for the funding tx in mempool BEFORE mining
#   - assert payment state=="Success" AND preimage, never grep prose
#   - wallets apply blocks lazily (2-min production sync windows):
#     wait_wallet_synced before any funding-dependent step
#   - cold data dirs sometimes need a second launch (wait_up relaunches)

REPO=${REPO:-$HOME/lampo-sim}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$REPO/sim-run}
SEED=${SEED:-42}
TMO=${TMO:-60}
KEEP_GOING=${KEEP_GOING:-0}
API_BASE=${API_BASE:-8100}
P2P_BASE=${P2P_BASE:-19900}
CORE_URL=${CORE_URL:-http://127.0.0.1:18332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
ART=${ART:-$SIMDIR/artifacts}

# Seeded RNG — same scheme as simulate.sh: every draw is a fresh
# `random.Random("$SEED:$tag")`, stateless and reproducible.
rand0() { # rand0 <tag> <n>  -> value in [0,n)
  python3 -c "import random;print(random.Random('$SEED:$1').randrange($2))"
}
rand_pick() { # rand_pick <tag> <item...>
  local tag=$1; shift
  local i=$(( $(rand0 "$tag" $#) + 1 ))
  echo "${!i}"
}
rand_amount() { # rand_amount <tag> <min> <max> -> log-uniform msat
  python3 -c "
import math, random
r = random.Random('$SEED:amt:$1')
print(int(math.exp(math.log($2) + (math.log($3)-math.log($2)) * r.random())))"
}

bcli() { # bitcoind RPC on the loaded `default` wallet
  curl -sS --max-time 30 --user "$CORE_USER:$CORE_PASS" \
    --data-binary "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" \
    "$CORE_URL/wallet/default"
}
bcres() { bcli "$@" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(json.dumps(d.get("result") if "result" in d else d.get("error")))' 2>/dev/null; }
mine() { local a; a=$(bcres getnewaddress | tr -d '"'); bcli generatetoaddress "[${1:-6},\"$a\"]" >/dev/null 2>&1; }

rpc() { curl -sS --max-time "$TMO" -X POST "http://127.0.0.1:$1/$2" -H 'content-type: application/json' -d "${3:-{\}}"; }
jqf() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
say() { echo "[$(date +%m-%d\ %H:%M:%S)] $*" | tee -a "$LOG"; }

API() { echo $(( API_BASE + ${IDX[$1]:-${1#n}} )); }
P2P() { echo $(( P2P_BASE + ${IDX[$1]:-${1#n}} )); }
node_dir() { echo "$SIMDIR/$1"; }
node_pid() { pgrep -f "lampod-cli --data-dir $SIMDIR/$1 " | head -1; }
node_log() { echo "$SIMDIR/$1/mh.log"; }
manager_file() { echo "$SIMDIR/$1/regtest/manager"; }         # LDK fs_store v1
monitors_dir() { echo "$SIMDIR/$1/regtest/monitors"; }        # one file per channel

collect_artifacts() { # $1 = tag
  local dir="$ART/$(date +%Y%m%d-%H%M%S)-$1" n
  mkdir -p "$dir"
  for n in "${ALLNODES[@]}"; do
    cp -r "$(node_dir "$n")" "$dir/" 2>/dev/null
    tail -c 200000 "$(node_log "$n")" > "$dir/$n-tail.log" 2>/dev/null
    rpc "$(API "$n")" getinfo > "$dir/$n-getinfo.json" 2>/dev/null
  done
  cp "${CSV:-$SIMDIR/results.csv}" "$LOG" "$dir/" 2>/dev/null
  bcres getmempoolinfo > "$dir/mempool.json" 2>/dev/null
  say "artifacts collected in $dir"
}
fail() { say "FAIL: $*"; collect_artifacts "$(echo "$*" | tr ' /' '__' | head -c 40)"; [ "$KEEP_GOING" = 1 ] || exit 2; }

start_node() { # $1 = name (writes conf; never deletes lampod.pid)
  local n=$1 dir; dir=$(node_dir "$n")
  mkdir -p "$dir/regtest"
  cat > "$dir/regtest/lampo.conf" <<EOF
network=regtest
port=$(P2P "$n")
announce-addr=127.0.0.1
api-host=http://127.0.0.1
api-port=$(API "$n")
backend=core
core-url=$CORE_URL
core-user=$CORE_USER
core-pass=$CORE_PASS
EOF
  setsid nohup "$BIN" --data-dir "$dir" --network regtest \
      > "$dir/mh.log" 2>&1 < /dev/null &
  disown 2>/dev/null || true
}
kill9()   { local p; p=$(node_pid "$1"); [ -n "$p" ] && kill -9   "$p" 2>/dev/null; }
killterm(){ local p; p=$(node_pid "$1"); [ -n "$p" ] && kill -TERM "$p" 2>/dev/null; }
killint() { local p; p=$(node_pid "$1"); [ -n "$p" ] && kill -INT  "$p" 2>/dev/null; }

wait_up() { # $1 name -> echoes node_id (relaunches: cold dirs need 2 tries)
  local n=$1 id
  for _ in $(seq 1 3); do
    for _ in $(seq 1 12); do
      sleep 5
      id=$(rpc "$(API "$n")" getinfo | jqf 'd["node_id"]')
      [ -n "$id" ] && { echo "$id"; return 0; }
    done
    start_node "$n"
  done
  return 1
}
wait_dead() { # $1 name [$2 timeout=60] -> 0 iff the process exits in time
  local deadline=$(( $(date +%s) + ${2:-60} ))
  while [ "$(date +%s)" -le "$deadline" ]; do
    [ -z "$(node_pid "$1")" ] && return 0
    sleep 2
  done
  return 1
}
api_dead() { [ -z "$(rpc "$(API "$1")" getinfo | jqf 'd.get("node_id","")')" ]; }

fund_node() { # $1 name $2 btc
  local addr
  addr=$(rpc "$(API "$1")" new_addr | jqf 'd["address"]')
  [ -n "$addr" ] || { say "no address from $1"; return 1; }
  bcres sendtoaddress "[\"$addr\", $2]" >/dev/null
  mine 6
}
open_channel() { # $1 from-name $2 to-name $3 to-id [$4 amount] [$5 push_msat]
  local from=$1 to=$2 id=$3 amt=${4:-1000000} push=${5:-0} resp sz
  rpc "$(API "$from")" connect "{\"node_id\":\"$id\",\"addr\":\"127.0.0.1\",\"port\":$(P2P "$to")}" >/dev/null
  # push_msat > 0 gives the PEER outbound liquidity: without it a dead-end
  # payee (hr) cannot pay anything back (RouteNotFound) — the 0-push trap
  # behind issue #566 / PR #569.
  resp=$(TMO=150 rpc "$(API "$from")" fundchannel \
    "{\"node_id\":\"$id\",\"addr\":\"127.0.0.1\",\"port\":$(P2P "$to"),\"amount\":$amt,\"public\":true,\"push_msat\":$push}")
  case "$resp" in
    "{"*) : ;;
    *) say "open_channel $from->$to non-JSON: $(echo "$resp" | head -c 200)"; return 1 ;;
  esac
  if echo "$resp" | jqf 'd.get("error",{}).get("message","")' | grep -q .; then
    say "open_channel $from->$to RPC error: $(echo "$resp" | head -c 300)"; return 1
  fi
  for _ in $(seq 1 20); do   # funding tx in mempool BEFORE mining (race lesson)
    sz=$(bcli getmempoolinfo | jqf 'd["result"]["size"]'); [ "${sz:-0}" -gt 0 ] 2>/dev/null && break; sleep 3
  done
  mine 8
}
ready_channels() { rpc "$(API "$1")" channels | jqf 'sum(1 for c in d["channels"] if c["ready"])'; }
channel_ids() { rpc "$(API "$1")" channels | jqf '" ".join(sorted(c["channel_id"] for c in d.get("channels",[])))'; }
# ready-only variant: baselines must ignore channels that are closing/closed
# (their entries retire asynchronously once the closing tx confirms).
ready_channel_ids() { rpc "$(API "$1")" channels | jqf '" ".join(sorted(c["channel_id"] for c in d.get("channels",[]) if c.get("ready")))'; }
peers_of() { rpc "$(API "$1")" getinfo | jqf 'd["peers"]'; }

wait_wallet_synced() { # [$1 timeout=420] over ALLNODES
  local deadline=$(( $(date +%s) + ${1:-420} )) n info h w
  for n in "${ALLNODES[@]}"; do
    while :; do
      info=$(rpc "$(API "$n")" getinfo)
      h=$(echo "$info" | jqf 'd.get("blockheight",-1)'); w=$(echo "$info" | jqf 'd.get("wallet_height",-2)')
      [ "$(( h - w ))" -le 1 ] && break
      [ "$(date +%s)" -gt "$deadline" ] && { say "wallet of $n behind: chain=$h wallet=$w"; return 1; }
      sleep 10
    done
  done
  say "all wallets synced to chain tip"
}

# --- health monitor on the log DELTA of a case ----------------------
log_marks() { # -> space-separated line counts, one per ALLNODES entry
  local n
  for n in "${ALLNODES[@]}"; do wc -l < "$(node_log "$n")" 2>/dev/null || echo 0; done | tr '\n' ' '
}
log_delta() { # log_delta <mark> <file>: lines after the mark. If the mark
  # points past EOF the file was truncated (start_node rewrites mh.log on
  # every relaunch) -> scan from the top instead of returning nothing.
  local mark=$1 file=$2 len
  len=$(wc -l < "$file" 2>/dev/null || echo 0)
  [ "${mark:-0}" -ge "$len" ] && mark=0
  tail -n +"$(( mark + 1 ))" "$file" 2>/dev/null
}
health_scan_since() { # $1 = marks from log_marks
  local n marks=($1) i=0 hits m
  for n in "${ALLNODES[@]}"; do
    m=${marks[$i]:-0}; i=$((i+1))
    hits=$(log_delta "$m" "$(node_log "$n")" | grep -hiE "panic|corrupt|invariant" | grep -v grep | head -3)
    [ -n "$hits" ] && { say "HEALTH($n): $hits"; return 1; }
  done
  return 0
}

save_ids() { # ID[...] map -> $SIMDIR/ids.env (safely re-sourceable)
  local n
  mkdir -p "$(dirname "$SIMDIR/ids.env")"
  : > "$SIMDIR/ids.env"
  for n in "${ALLNODES[@]}"; do echo "ID[$n]=${ID[$n]}" >> "$SIMDIR/ids.env"; done
}
load_ids() { # fills ID from $SIMDIR/ids.env; returns 1 if missing/incomplete
  [ -f "$SIMDIR/ids.env" ] || return 1
  local n
  source "$SIMDIR/ids.env"
  for n in "${ALLNODES[@]}"; do [ -n "${ID[$n]:-}" ] || return 1; done
  return 0
}

# --- multihop payment with hop-structure assertions ------------------
# mh_pay <tag> <src> <dst> <amount_msat> <method> -> 0 iff
#   state=="Success" AND preimage AND (keysend OR (hops>=2 AND relay hm on path))
# Appends one row to $CSV. Requires ID[] filled and $BG unset-safe.
mh_pay() {
  local tag=$1 src=$2 dst=$3 amt=$4 m=$5 res state pre hops relay_ok dur hfee inv off
  local t0=$(( $(date +%s) )); res=""
  case $m in
    invoice)
      inv=$(TMO=30 rpc "$(API "$dst")" invoice "{\"amount_msat\":$amt,\"description\":\"$tag\"}" | jqf 'd.get("bolt11","")')
      [ -n "$inv" ] || { say "$tag: $dst issued no invoice"; echo "$(date -Iseconds),$tag,$src,$dst,$m,$amt,NoInvoice,,$(( $(date +%s)-t0 )), , " >> "$CSV"; return 1; }
      res=$(TMO=120 rpc "$(API "$src")" pay "{\"invoice_str\":\"$inv\"}")
      ;;
    offer)
      off=$(TMO=30 rpc "$(API "$dst")" offer "{\"amount_msat\":$amt,\"description\":\"$tag\"}" | jqf 'd.get("bolt12","")')
      [ -n "$off" ] || { say "$tag: $dst issued no offer"; echo "$(date -Iseconds),$tag,$src,$dst,$m,$amt,NoOffer,,$(( $(date +%s)-t0 )), , " >> "$CSV"; return 1; }
      res=$(TMO=120 rpc "$(API "$src")" pay "{\"invoice_str\":\"$off\",\"amount\":$amt}")
      ;;
    keysend)
      res=$(TMO=120 rpc "$(API "$src")" keysend "{\"destination\":\"${ID[$dst]}\",\"amount_msat\":$amt}")
      ;;
  esac
  dur=$(( $(date +%s) - t0 ))
  state=$(echo "$res" | jqf 'd.get("state","")')
  pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  hops=$(echo "$res" | jqf 'len(d.get("path",[]))')
  relay_ok=$(echo "$res" | jqf 'int("'"${ID[hm]:-x}"'" in [h.get("node_id","") for h in d.get("path",[])])')
  hfee=$(echo "$res" | jqf 'max([h.get("hop_fee_msat",0) for h in d.get("path",[])])')
  echo "$(date -Iseconds),$tag,$src,$dst,$m,$amt,${state:-none},${pre:0:16},$dur,${hops:-0},${relay_ok:-0}" >> "$CSV"
  if [ "${state:-}" != Success ] || [ -z "$pre" ]; then
    say "$tag FAIL: $src->$dst ($m ${amt}msat) state=${state:-none} hops=${hops:-?} raw=$(echo "$res" | head -c 200)"
    return 1
  fi
  if [ "$m" != keysend ] && { [ "${hops:-0}" -lt 2 ] || [ "${relay_ok:-0}" != 1 ]; }; then
    say "$tag FAIL: $src->$dst ($m) route structure wrong (hops=${hops:-?} relay_ok=${relay_ok:-?}) — direct route in a dead-end topology?!"
    return 1
  fi
  if [ "$m" != keysend ]; then
    say "$tag OK: $src->$dst ($m ${amt}msat, ${dur}s, hops=$hops via hm, max_hop_fee=${hfee:-0}msat, preimage ${pre:0:8}..)"
  else
    say "$tag OK: $src->$dst (keysend ${amt}msat, ${dur}s, preimage ${pre:0:8}..)"
  fi
  return 0
}

# --- mh cluster (hs -- hm -- hr dead-end chain) -----------------------
# Reuses a healthy cluster, else builds one. fill ID via save_ids.
cluster_up() {
  local n id
  for n in "${ALLNODES[@]}"; do
    id=$(rpc "$(API "$n")" getinfo | jqf 'd["node_id"]')
    [ "$id" = "${ID[$n]:-}" ] && [ -n "$id" ] || return 1
  done
  return 0
}
ensure_cluster() {
  if load_ids && cluster_up && \
     [ "$(ready_channels hs)" -ge 1 ] && [ "$(ready_channels hm)" -ge 2 ] && [ "$(ready_channels hr)" -ge 1 ]; then
    say "reusing mh cluster (hs=${ID[hs]:0:12}.. hm=${ID[hm]:0:12}.. hr=${ID[hr]:0:12}..)"
    return 0
  fi
  say "building mh cluster: fund hs/hm/hr, open hs->hm and hm->hr"
  local leftovers n deadline ok
  leftovers=$(pgrep -f "lampod-cli --data-dir $MHDIR/" || true)
  [ -n "$leftovers" ] && { say "killing stale mh nodes: $leftovers"; kill -9 $leftovers; sleep 3; }
  for n in "${ALLNODES[@]}"; do
    start_node "$n"
    ID[$n]=$(wait_up "$n") || { say "$n never came up"; return 1; }
    say "  $n = ${ID[$n]:0:16}.. (api :$(API "$n") p2p :$(P2P "$n"))"
  done
  save_ids
  for n in "${ALLNODES[@]}"; do fund_node "$n" 0.05 || return 1; done
  sleep 140                     # production wallet sync runs on 2-min windows
  wait_wallet_synced 420 || return 1
  # Push half the capacity to the peer on both channels so BOTH dead ends
  # (hm on hs-hm, hr on hm-hr) hold outbound liquidity and can route
  # payments BACK (reverse multihop, a2) — otherwise RouteNotFound.
  local push_msat=$(( MH_CHANNEL_AMT_SATS * 1000 / 2 ))
  open_channel hs hm "${ID[hm]}" "${MH_CHANNEL_AMT_SATS:-1000000}" "$push_msat" || return 1
  open_channel hm hr "${ID[hr]}" "${MH_CHANNEL_AMT_SATS:-1000000}" "$push_msat" || return 1
  sleep 30
  wait_wallet_synced 300 || true
  deadline=$(( $(date +%s) + 180 )); ok=1
  while :; do
    ok=1
    [ "$(ready_channels hs)" -ge 1 ] || ok=0
    [ "$(ready_channels hm)" -ge 2 ] || ok=0
    [ "$(ready_channels hr)" -ge 1 ] || ok=0
    [ "$ok" = 1 ] && break
    [ "$(date +%s)" -gt "$deadline" ] && { say "mh channels never ready (hs=$(ready_channels hs) hm=$(ready_channels hm) hr=$(ready_channels hr))"; return 1; }
    # Chain movement also nudges LDK monitors to rebroadcast pending
    # funding txs (lesson from issue #572: a rejected first broadcast
    # only heals after new blocks arrive).
    mine 2
    sleep 15
  done
  say "mh cluster ready; waiting 150s for gossip (BOLT12 precondition)"
  sleep 150
  return 0
}
