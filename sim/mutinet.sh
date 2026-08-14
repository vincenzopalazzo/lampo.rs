#!/usr/bin/env bash
#
# mutinet.sh — the multinet (mutinynet/signet) leg of the lampo simulation.
#
# Deploys TWO lampo nodes (m1, m2) built from the sim worktree on the
# debian server against the mutinynet bitcoind (docker mutiny-bitcoind-1,
# RPC 127.0.0.1:38332), funds m1 from the mutinynet faucet, opens one
# channel m1->m2 and then soaks: endless alternating invoice payments
# (m1 -> m2 and back, rebalancing directions) + getinfo health polling,
# log scanning, artifact collection on failure.
#
# Difference vs regtest simulate.sh: real signet — 30s blocks, real
# mempool/fees, no mining control. We do NOT reorg/storm here; the point
# is watching updated-main soak on a live-ish network.
#
# Env: BIN, CORE_URL (default http://127.0.0.1:38332), CORE_USER/PASS,
#      API_BASE (8110), P2P_BASE (19910), ROUNDS (0=endless), WAIT (300s
#      between payments), FAUCET (https://faucet.mutinynet.com)
set -uo pipefail

BIN=${BIN:-$HOME/lampo-sim/target/release/lampod-cli}
RUN=${RUN:-$HOME/lampo-sim/mutinet}
CORE_URL=${CORE_URL:-http://127.0.0.1:38332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
API_BASE=${API_BASE:-8110}
P2P_BASE=${P2P_BASE:-19910}
ROUNDS=${ROUNDS:-0}
WAIT=${WAIT:-300}
FAUCET=${FAUCET:-https://faucet.mutinynet.com}
LOG=$RUN/mutinet.log

bcli() { curl -s --max-time 20 --user "$CORE_USER:$CORE_PASS" \
  --data-binary "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" "$CORE_URL"; }
bcres() { bcli "$@" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(json.dumps(d.get("result") if d.get("result") is not None else d.get("error")))' 2>/dev/null; }
rpc() { curl -sS --max-time 60 -X POST "http://127.0.0.1:$1/$2" -H 'content-type: application/json' -d "${3:-{\}}"; }
jqf() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
say() { echo "[$(date +%m-%d\ %H:%M:%S)] $*" | tee -a "$LOG"; }
api() { echo $((API_BASE + $1)); }
p2p() { echo $((P2P_BASE + $1)); }

start_node() { # $1 = 1|2
  local dir="$RUN/m$1"
  mkdir -p "$dir/signet"
  cat > "$dir/signet/lampo.conf" <<EOF
network=signet
port=$(p2p "$1")
announce-addr=127.0.0.1
api-host=http://127.0.0.1
api-port=$(api "$1")
backend=core
core-url=$CORE_URL
core-user=$CORE_USER
core-pass=$CORE_PASS
EOF
  setsid "$BIN" --data-dir "$dir" --network signet \
      > "$dir/mh.log" 2>&1 < /dev/null &
  disown 2>/dev/null || true
}

wait_up() { # $1 = 1|2 -> echo node_id
  for _ in $(seq 1 3); do
    for _ in $(seq 1 12); do
      sleep 5
      local id; id=$(rpc "$(api "$1")" getinfo | jqf 'd["node_id"]')
      [ -n "$id" ] && { echo "$id"; return 0; }
    done
    start_node "$1"
  done
  return 1
}

collect_artifacts() {
  local dir="$RUN/artifacts/$(date +%Y%m%d-%H%M%S)-$1"
  mkdir -p "$dir"
  cp -r "$RUN"/m1 "$RUN"/m2 "$dir/" 2>/dev/null
  cp "$LOG" "$dir/" 2>/dev/null
  say "artifacts in $dir"
}
fail() { say "FAIL: $*"; collect_artifacts "$(echo "$*" | tr ' /' '__' | head -c 40)"; exit 2; }

health() {
  rpc "$(api 1)" getinfo | jqf 'd["blockheight"]' | grep -qE '^[0-9]+$' || return 1
  rpc "$(api 2)" getinfo | jqf 'd["blockheight"]' | grep -qE '^[0-9]+$' || return 1
  grep -hiE "panic|corrupt|invariant" "$RUN"/m*/mh.log 2>/dev/null | head -3 | while read -r l; do say "HEALTH: $l"; return 1; done
}

# ============================ main ====================================
mkdir -p "$RUN"
: > "$LOG"
say "multinet soak: bin=$BIN rounds=$ROUNDS wait=${WAIT}s core=$CORE_URL"

# leftover nodes would hold the ports
P=muti; pkill -9 -f "lampod-cli --data-dir $RUN" 2>/dev/null; sleep 2

say "starting m1, m2 on signet"
start_node 1; start_node 2
ID1=$(wait_up 1) || fail "m1 never came up"
ID2=$(wait_up 2) || fail "m2 never came up"
say "  m1=${ID1:0:16}… :$(api 1)   m2=${ID2:0:16}… :$(api 2)"

# --- fund m1 from the faucet (once) ---
FUNDED=0
for attempt in 1 2 3; do
  addr=$(rpc "$(api 1)" new_addr | jqf 'd["address"]')
  [ -n "$addr" ] || { say "no address from m1 (attempt $attempt)"; sleep 10; continue; }
  say "requesting faucet funds for $addr (attempt $attempt)"
  code=$(curl -s -o /tmp/faucet.out -w "%{http_code}" --max-time 30 \
     -X POST "$FAUCET/api/onchain" -H 'content-type: application/json' \
     -d "{\"address\":\"$addr\"}" 2>/dev/null || echo 000)
  say "faucet http=$code body=$(head -c 120 /tmp/faucet.out 2>/dev/null)"
  # wait for the funds to appear (wallet syncs every 2 min)
  for _ in $(seq 1 20); do
    sleep 30
    funds=$(rpc "$(api 1)" funds | jqf 'sum(int(t["amount_msat"]) for t in d.get("transactions",[]) if int(t.get("amount_msat",0))>0)')
    [ "${funds:-0}" -gt 10000000 ] && { FUNDED=1; break; }
  done
  [ "$FUNDED" = 1 ] && break
done
[ "$FUNDED" = 1 ] || fail "m1 never got faucet funds"
say "m1 funded ($funds msat visible)"

# --- open one channel m1 -> m2 ---
say "connecting + opening channel m1->m2 (2_000_000 sat)"
rpc "$(api 1)" connect "{\"node_id\":\"$ID2\",\"addr\":\"127.0.0.1\",\"port\":$(p2p 2)}" >/dev/null
resp=$(rpc "$(api 1)" fundchannel "{\"node_id\":\"$ID2\",\"addr\":\"127.0.0.1\",\"port\":$(p2p 2),\"amount\":2000000,\"public\":true}")
case "$resp" in "{"*) : ;; *) fail "fundchannel non-JSON: $resp" ;; esac
echo "$resp" | jqf 'd.get("error",{}).get("message","")' | grep -q . && fail "fundchannel error: $resp"

# channel needs 6 signet confirmations (~30s blocks) + wallet sync
say "waiting for channel to mature (up to 12 min)"
for _ in $(seq 1 24); do
  sleep 30
  ready=$(rpc "$(api 1)" channels | jqf 'sum(1 for c in d.get("channels",[]) if c.get("ready"))')
  health >/dev/null 2>&1 || true
  [ "${ready:-0}" -ge 1 ] && break
done
[ "${ready:-0}" -ge 1 ] || fail "channel m1->m2 never became ready (ready=$ready)"
say "channel ready; starting payment soak"

# --- endless alternating payments ---
r=0
while :; do
  r=$((r+1))
  if [ $(( r % 2 )) = 1 ]; then src=1; dst=2; else src=2; dst=1; fi
  amt=$(( 20000 + (r % 7) * 10000 ))
  inv=$(rpc "$(api "$dst")" invoice "{\"amount_msat\":$amt,\"description\":\"multinet round $r\"}" | jqf 'd.get("bolt11","")')
  [ -n "$inv" ] || { say "round $r: m$dst issued no invoice"; sleep 60; continue; }
  res=$(rpc "$(api "$src")" pay "{\"invoice_str\":\"$inv\"}")
  state=$(echo "$res" | jqf 'd.get("state","")')
  pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  if [ "$state" = "Success" ] && [ -n "$pre" ]; then
    say "round $r OK: m$src -> m$dst ${amt}msat (preimage ${pre:0:8}…)"
  else
    say "round $r FAIL: m$src -> m$dst state=${state:-none} raw=$(echo "$res" | head -c 200)"
    fail "round $r payment failed"
  fi
  health || fail "health scan tripped after round $r"
  [ "$ROUNDS" != 0 ] && [ "$r" -ge "$ROUNDS" ] && break
  sleep "$WAIT"
done
say "MULTINET SOAK COMPLETE after $r rounds"
