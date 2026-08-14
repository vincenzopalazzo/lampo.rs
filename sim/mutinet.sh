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
  # API liveness with retries: a single transient empty/slow getinfo
  # (e.g. colliding with a monitor persist) must not kill the soak.
  local i
  for i in 1 2 3; do
    if rpc "$(api 1)" getinfo | jqf 'd["blockheight"]' | grep -qE '^[0-9]+$' && \
       rpc "$(api 2)" getinfo | jqf 'd["blockheight"]' | grep -qE '^[0-9]+$'; then
      return 0
    fi
    sleep 5
  done
  return 1
}

log_scan() {
  # NOTE: a `return` inside `while read` on a pipe runs in a subshell and
  # never propagates — collect hits to a file first.
  local hits=/tmp/mutinet-health.$$
  grep -hiE "panic|corrupt|invariant" "$RUN"/m*/mh.log 2>/dev/null | head -3 >"$hits" || true
  if [ -s "$hits" ]; then
    while read -r l; do say "HEALTH: $l"; done <"$hits"
    rm -f "$hits"; return 1
  fi
  rm -f "$hits"; return 0
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
# The hosted faucet uses L402: GET /api/l402 -> {invoice(500msat), token};
# pay the invoice with ANY lightning node that can route on mutinynet, poll
# /api/l402/check until "settled", then the token unlocks POST /api/onchain.
# Set FAUCET_TOKEN directly if you already hold a settled token.
# PAY_NODE_URL lets you pick the node used to pay the challenge
# (default: the old node-mut-r API).
FUNDED=0
# Channel size in sats. The old default (2M) is regtest-scale; on mutinynet
# pick something the funding wallet can actually afford (e.g. 30000).
CHANNEL_SAT=${CHANNEL_SAT:-2000000}
# m1 may already be funded on-chain (e.g. manually from the bitcoind wallet,
# or left over from a previous run) — skip the faucet entirely then.
# The wallet syncs on a 2-min cadence, so retry the check for a few minutes.
for _ in $(seq 1 8); do
  funds_pre=$(rpc "$(api 1)" funds | jqf 'sum(int(t["amount_msat"]) for t in d.get("transactions",[]) if int(t.get("amount_msat",0))>0)')
  [ "${funds_pre:-0}" -gt $((CHANNEL_SAT * 800)) ] && break
  sleep 30
done
if [ "${funds_pre:-0}" -gt $((CHANNEL_SAT * 800)) ]; then
  FUNDED=1; funds=$funds_pre
  say "m1 already funded ($funds msat visible) — skipping faucet"
fi
fund_via_faucet() {
  local addr=$1
  if [ -n "${FAUCET_TOKEN:-}" ]; then
    local code
    code=$(curl -s -o /tmp/faucet.out -w "%{http_code}" --max-time 30 \
      -X POST "$FAUCET/api/onchain" -H 'content-type: application/json' \
      -H "Authorization: Bearer $FAUCET_TOKEN" \
      -d "{\"sats\":1000000,\"address\":\"$addr\"}")
    say "faucet(bearer) http=$code body=$(head -c 120 /tmp/faucet.out)"
    [ "$code" = 200 ] && return 0
    return 1
  fi
  local ch inv tok st
  ch=$(curl -s --max-time 15 "$FAUCET/api/l402") || return 1
  inv=$(echo "$ch" | jqf 'd.get("invoice","")'); tok=$(echo "$ch" | jqf 'd.get("token","")')
  [ -n "$inv" ] && [ -n "$tok" ] || { say "faucet: no L402 challenge"; return 1; }
  local pres
  pres=$(curl -sS --max-time 90 -X POST "${PAY_NODE_URL:-http://127.0.0.1:7996}/pay" \
      -H 'content-type: application/json' -d "{\"invoice_str\":\"$inv\"}" 2>/dev/null | head -c 120)
  echo "$pres" | grep -q '"payment_preimage"' || { say "faucet: challenge unpaid ($pres)"; return 1; }
  for _ in $(seq 1 12); do
    sleep 5
    st=$(curl -s --max-time 10 "$FAUCET/api/l402/check?token=$tok" | jqf 'd.get("status","")')
    [ "$st" = "settled" ] && break
  done
  [ "$st" = "settled" ] || { say "faucet: L402 never settled"; return 1; }
  local code2
  code2=$(curl -s -o /tmp/faucet.out -w "%{http_code}" --max-time 30 \
    -X POST "$FAUCET/api/onchain" -H 'content-type: application/json' \
    -H "Authorization: Bearer $tok" \
    -d "{\"sats\":1000000,\"address\":\"$addr\"}")
  say "faucet(l402) http=$code2 body=$(head -c 120 /tmp/faucet.out)"
  [ "$code2" = 200 ]
}
# Resume: if m1 already has a ready channel (e.g. harness relaunch after a
# failure while the channel persisted on disk), skip funding entirely and
# go straight to the payment soak. Reconnect the link in case the nodes
# were restarted.
RESUME=0
for _ in $(seq 1 10); do
  ready=$(rpc "$(api 1)" channels | jqf 'sum(1 for c in d.get("channels",[]) if c.get("ready"))')
  [ "${ready:-0}" -ge 1 ] && { RESUME=1; break; }
  sleep 30
done
if [ "$RESUME" = 1 ]; then
  say "m1 already has a ready channel — resuming payment soak"
  rpc "$(api 1)" connect "{\"node_id\":\"$ID2\",\"addr\":\"127.0.0.1\",\"port\":$(p2p 2)}" >/dev/null 2>&1
fi
if [ "$RESUME" != 1 ]; then
for attempt in 1 2 3; do
  [ "$FUNDED" = 1 ] && break
  addr=$(rpc "$(api 1)" new_addr | jqf 'd["address"]')
  [ -n "$addr" ] || { say "no address from m1 (attempt $attempt)"; sleep 10; continue; }
  fund_via_faucet "$addr" || { sleep 30; continue; }
  # wait for the funds to appear (wallet syncs every 2 min)
  for _ in $(seq 1 20); do
    sleep 30
    funds=$(rpc "$(api 1)" funds | jqf 'sum(int(t["amount_msat"]) for t in d.get("transactions",[]) if int(t.get("amount_msat",0))>0)')
    [ "${funds:-0}" -gt 10000000 ] && { FUNDED=1; break; }
  done
  [ "$FUNDED" = 1 ] && break
done
if [ "$FUNDED" != 1 ]; then
  say "no faucet funds — falling back to CONNECTIVITY SOAK (sync + peering + health only; payments need FAUCET_TOKEN or a payable mutinynet node)"
  rpc "$(api 1)" connect "{\"node_id\":\"$ID2\",\"addr\":\"127.0.0.1\",\"port\":$(p2p 2)}" >/dev/null 2>&1
  while :; do
    sleep 60
    h1=$(rpc "$(api 1)" getinfo | jqf 'd.get("blockheight",-1)'); p1=$(rpc "$(api 1)" getinfo | jqf 'd.get("peers",-1)')
    h2=$(rpc "$(api 2)" getinfo | jqf 'd.get("blockheight",-1)')
    say "conn-soak: m1 h=$h1 peers=$p1 | m2 h=$h2"
    grep -hiE "panic|corrupt|invariant" "$RUN"/m*/mh.log 2>/dev/null | head -2 | while read -r l; do say "HEALTH: $l"; done
    [ "${h1:-0}" -lt 0 ] && fail "m1 api dead"
    [ "${h2:-0}" -lt 0 ] && fail "m2 api dead"
  done
fi
say "m1 funded ($funds msat visible)"

# --- open one channel m1 -> m2 ---
say "connecting + opening channel m1->m2 (${CHANNEL_SAT} sat)"
rpc "$(api 1)" connect "{\"node_id\":\"$ID2\",\"addr\":\"127.0.0.1\",\"port\":$(p2p 2)}" >/dev/null
resp=$(rpc "$(api 1)" fundchannel "{\"node_id\":\"$ID2\",\"addr\":\"127.0.0.1\",\"port\":$(p2p 2),\"amount\":$CHANNEL_SAT,\"public\":true}")
case "$resp" in "{"*) : ;; *) fail "fundchannel non-JSON: $resp" ;; esac
echo "$resp" | jqf 'd.get("error",{}).get("message","")' | grep -q . && fail "fundchannel error: $resp"

# channel needs 6 signet confirmations (~30s blocks) + wallet sync
say "waiting for channel to mature (up to 12 min)"
for _ in $(seq 1 24); do
  sleep 30
  ready=$(rpc "$(api 1)" channels | jqf 'sum(1 for c in d.get("channels",[]) if c.get("ready"))')
  health >/dev/null 2>&1 || true
  log_scan >/dev/null 2>&1 || true
  [ "${ready:-0}" -ge 1 ] && break
done
[ "${ready:-0}" -ge 1 ] || fail "channel m1->m2 never became ready (ready=$ready)"
fi
say "channel ready; starting payment soak"

# --- endless alternating payments ---
r=0
PREV_AMT=0
while :; do
  r=$((r+1))
  if [ $(( r % 2 )) = 1 ]; then src=1; dst=2; else src=2; dst=1; fi
  # Odd rounds m1 sends a fresh amount; even rounds m2 pays back EXACTLY
  # what it just received. With a 0-push channel any larger even-round
  # amount fails with SendingFailed(RouteNotFound) — the router's way of
  # saying "no outbound balance" — which is expected LN behaviour, not a
  # node bug, but it would kill the soak every second round.
  if [ $(( r % 2 )) = 1 ]; then
    amt=$(( 5000 + (r % 7) * 2500 ))
  else
    amt=$PREV_AMT
  fi
  PREV_AMT=$amt
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
  log_scan || fail "log scan tripped after round $r"
  [ "$ROUNDS" != 0 ] && [ "$r" -ge "$ROUNDS" ] && break
  sleep "$WAIT"
done
say "MULTINET SOAK COMPLETE after $r rounds"
