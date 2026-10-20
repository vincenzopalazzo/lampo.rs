#!/usr/bin/env bash
#
# simulations/splice.sh — two-node splice QA.
#
# sa opens a channel to sb, splices in, refuses a second splice while one
# is pending, bumps the fee, splices out, then pays across the channel.
# Asserts RPC fields and the channel value, never log prose.
#
# Ports API 8611-8612 / P2P 20611-20612. Data dir defaults to
# $HOME/lampo-splice-qa, not the soak tree under $HOME/lampo-sim.
# Regtest mines. Signet (mutinynet on :38332) waits for the chain that
# is already running and takes coins from the faucet.
#
# Env: REPO BIN SIMDIR CORE_URL SEED CHANNEL_SATS(150000)
#      SPLICE_IN_SATS(20000) SPLICE_OUT_SATS(10000)
#      FAUCET PAY_NODE_URL FAUCET_TOKEN
#
# Usage:
#   REPO=$HOME/lampo-splice-qa BIN=$REPO/target/release/lampod-cli \
#     ./simulations/splice.sh
set -uo pipefail

REPO=${REPO:-$HOME/lampo-splice-qa}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$HOME/lampo-splice-qa/sim-run}
API_BASE=${API_BASE:-8610}
P2P_BASE=${P2P_BASE:-20610}
CORE_URL=${CORE_URL:-http://127.0.0.1:38332}
CHANNEL_SATS=${CHANNEL_SATS:-150000}
SPLICE_IN_SATS=${SPLICE_IN_SATS:-20000}
SPLICE_OUT_SATS=${SPLICE_OUT_SATS:-10000}
FAUCET=${FAUCET:-https://faucet.mutinynet.com}
PAY_NODE_URL=${PAY_NODE_URL:-http://127.0.0.1:7996}
LOG=${LOG:-$SIMDIR/splice.log}
CSV=${CSV:-$SIMDIR/splice.csv}

source "$(dirname "$0")/lib.sh"

declare -A IDX=([sa]=1 [sb]=2)
ALLNODES=(sa sb)
declare -A ID=()

mkdir -p "$SIMDIR" "$ART"
: > "$LOG"
echo "ts,step,detail" > "$CSV"

row() { echo "$(date -Iseconds),$1,$2" >> "$CSV"; say "$1 $2"; }

chain_rpc() {
  curl -sS --max-time 30 --user "$CORE_USER:$CORE_PASS" \
    --data-binary "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" \
    "$CORE_URL"
}
in_mempool() { chain_rpc getrawmempool | grep -q "$1"; }
advance() {
  if [ "$CHAIN" = regtest ]; then
    mine "${1:-6}"
  else
    sleep $(( ${1:-6} * 20 ))
  fi
}

say "splice QA: bin=$BIN simdir=$SIMDIR core=$CORE_URL seed=$SEED"
[ -x "$BIN" ] || { say "binary missing: $BIN"; exit 1; }
# Current lampod talks to bitcoind through the chain plugin.
if [ -z "${LAMPO_PLUGINS:-}" ]; then
  plugin_guess="$(dirname "$BIN")/examples/lampo-bitcoind"
  if [ -x "$plugin_guess" ]; then
    export LAMPO_PLUGINS="$plugin_guess"
  fi
fi
say "plugins=${LAMPO_PLUGINS:-none}"
[ -n "${LAMPO_PLUGINS:-}" ] || { say "lampod needs the bitcoind plugin (LAMPO_PLUGINS)"; exit 1; }
CHAIN=$(chain_rpc getblockchaininfo | jqf 'd["result"]["chain"]')
case "$CHAIN" in
  regtest|signet) ;;
  *) say "bitcoind at $CORE_URL chain=${CHAIN:-unknown}"; exit 1 ;;
esac
NETWORK=$CHAIN
if [ "$CHAIN" = signet ]; then
  REINDEX=$(chain_rpc getblockchaininfo | jqf 'd["result"]["blocks"]')
  export REINDEX
fi
CONFIRM_WAIT=${CONFIRM_WAIT:-$([ "$CHAIN" = signet ] && echo 1200 || echo 300)}
say "chain=$CHAIN confirm_wait=${CONFIRM_WAIT}s"

GUARD_PIDFILE="$SIMDIR/harness.pid"
if [ -f "$GUARD_PIDFILE" ]; then
  gp=$(cat "$GUARD_PIDFILE" 2>/dev/null || true)
  if [ -n "${gp:-}" ] && [ -d "/proc/$gp" ] && grep -qa "splice.sh" "/proc/$gp/cmdline" 2>/dev/null; then
    say "another splice.sh is already running (pid $gp)"
    exit 3
  fi
fi
echo $$ > "$GUARD_PIDFILE"
trap 'rm -f "$GUARD_PIDFILE"' EXIT

for n in "${ALLNODES[@]}"; do
  ss -ltn 2>/dev/null | grep -q ":$(API "$n") " && { say "port :$(API "$n") busy"; exit 3; }
done

cleanup_nodes() {
  local n
  for n in "${ALLNODES[@]}"; do killterm "$n"; done
  for n in "${ALLNODES[@]}"; do wait_dead "$n" 30 || kill9 "$n"; done
}

fund_signet() { # $1 name. Pays the mutinynet L402 from a node already on this signet.
  local n=$1 addr code ch inv tok st pres
  addr=$(rpc "$(API "$n")" new_addr | jqf 'd["address"]')
  [ -n "$addr" ] || return 1
  if [ -n "${FAUCET_TOKEN:-}" ]; then
    code=$(curl -s -o /tmp/splice-faucet.out -w "%{http_code}" --max-time 30 \
      -X POST "$FAUCET/api/onchain" -H 'content-type: application/json' \
      -H "Authorization: Bearer $FAUCET_TOKEN" \
      -d "{\"sats\":1000000,\"address\":\"$addr\"}")
    say "faucet bearer http=$code"
    [ "$code" = 200 ] || return 1
  else
    ch=$(curl -s --max-time 20 "$FAUCET/api/l402") || return 1
    inv=$(echo "$ch" | jqf 'd.get("invoice","")')
    tok=$(echo "$ch" | jqf 'd.get("token","")')
    [ -n "$inv" ] && [ -n "$tok" ] || { say "faucet: no L402 challenge"; return 1; }
    pres=$(curl -sS --max-time 120 -X POST "$PAY_NODE_URL/pay" \
      -H 'content-type: application/json' -d "{\"invoice_str\":\"$inv\"}" | head -c 200)
    echo "$pres" | grep -q '"payment_preimage"' || {
      pres=$(curl -sS --max-time 120 -X POST "http://127.0.0.1:8111/pay" \
        -H 'content-type: application/json' -d "{\"invoice_str\":\"$inv\"}" | head -c 200)
    }
    echo "$pres" | grep -q '"payment_preimage"' || { say "faucet challenge unpaid ($pres)"; return 1; }
    st=""
    for _ in $(seq 1 12); do
      sleep 5
      st=$(curl -s --max-time 15 "$FAUCET/api/l402/check?token=$tok" | jqf 'd.get("status","")')
      [ "$st" = "settled" ] && break
    done
    [ "$st" = "settled" ] || { say "faucet L402 never settled"; return 1; }
    code=$(curl -s -o /tmp/splice-faucet.out -w "%{http_code}" --max-time 30 \
      -X POST "$FAUCET/api/onchain" -H 'content-type: application/json' \
      -H "Authorization: Bearer $tok" \
      -d "{\"sats\":1000000,\"address\":\"$addr\"}")
    say "faucet l402 http=$code body=$(head -c 120 /tmp/splice-faucet.out)"
    [ "$code" = 200 ] || return 1
  fi
  local funds=0
  for _ in $(seq 1 24); do
    sleep 30
    funds=$(rpc "$(API "$n")" funds | jqf 'sum(int(t.get("amount_msat",0)) for t in d.get("transactions",[]) if int(t.get("amount_msat",0))>0)')
    [ "${funds:-0}" -gt 200000000 ] && return 0
  done
  say "$n still unfunded ($funds msat)"
  return 1
}

wait_node() { # signet startup is slower than wait_up's one minute
  local n=$1 id
  for _ in $(seq 1 60); do
    sleep 5
    id=$(rpc "$(API "$n")" getinfo 2>/dev/null | jqf 'd.get("node_id","")')
    [ -n "$id" ] && { echo "$id"; return 0; }
  done
  return 1
}

# A new wallet makes lampod write the mnemonic and exit. The next start
# is the daemon. An existing wallet stays up on the first start.
launch_node() {
  local n=$1 id
  start_node "$n"
  local i
  # A new wallet writes the mnemonic and exits. On a loaded host that
  # can take well over 20s, so wait until it exits or the API answers.
  for i in $(seq 1 180); do
    if [ -z "$(node_pid "$n")" ]; then
      start_node "$n"
      return 0
    fi
    id=$(rpc "$(API "$n")" getinfo 2>/dev/null | jqf 'd.get("node_id","")' || true)
    [ -n "$id" ] && return 0
    sleep 1
  done
}

say "starting sa and sb on $CHAIN reindex=${REINDEX:-none}"
for n in "${ALLNODES[@]}"; do
  launch_node "$n"
  ID[$n]=$(wait_node "$n") || { fail "$n never came up"; exit 2; }
  say "  $n = ${ID[$n]:0:16}.. api :$(API "$n") p2p :$(P2P "$n")"
done
if [ "$CHAIN" = regtest ]; then
  for n in "${ALLNODES[@]}"; do fund_node "$n" 0.05 || { fail "fund $n"; exit 2; }; done
  sleep 140
else
  fund_signet sa || { fail "signet faucet did not fund sa"; exit 2; }
fi
wait_wallet_synced 420 || { fail "wallets never synced"; exit 2; }

if [ "$CHAIN" = regtest ]; then
  open_channel sa sb "${ID[sb]}" "$CHANNEL_SATS" 0 || { fail "open sa->sb"; exit 2; }
else
  rpc "$(API sa)" connect "{\"node_id\":\"${ID[sb]}\",\"addr\":\"127.0.0.1\",\"port\":$(P2P sb)}" >/dev/null
  open_resp=$(TMO=180 rpc "$(API sa)" fundchannel \
    "{\"node_id\":\"${ID[sb]}\",\"addr\":\"127.0.0.1\",\"port\":$(P2P sb),\"amount\":$CHANNEL_SATS,\"public\":false,\"push_msat\":0}")
  ferr=$(fundchannel_error "$open_resp")
  [ -z "$ferr" ] || { fail "fundchannel: $ferr"; exit 2; }
  say "fundchannel tx $(echo "$open_resp" | jqf 'd.get("txid","")')"
fi
deadline=$(( $(date +%s) + CONFIRM_WAIT ))
while :; do
  [ "$(ready_channels sa)" -ge 1 ] && [ "$(ready_channels sb)" -ge 1 ] && break
  [ "$(date +%s)" -gt "$deadline" ] && { fail "channel never ready"; exit 2; }
  advance 1
  sleep 15
done
base=$(rpc "$(API sa)" channels | jqf 'd["channels"][0]["amount"]')
row open "amount=$base"
[ "$base" = "$CHANNEL_SATS" ] || { fail "opened amount $base, wanted $CHANNEL_SATS"; exit 2; }

bump=$(rpc "$(API sa)" bumpchannelfee "{\"node_id\":\"${ID[sb]}\"}")
msg=$(fundchannel_error "$bump")
echo "$msg" | grep -q "no pending splice" || { fail "bump with nothing pending: $msg"; exit 2; }
row bump-empty "ok"

first=$(TMO=150 rpc "$(API sa)" splicein "{\"node_id\":\"${ID[sb]}\",\"amount\":$SPLICE_IN_SATS}")
ferr=$(fundchannel_error "$first")
[ -z "$ferr" ] || { fail "splicein: $ferr"; exit 2; }
txid1=$(echo "$first" | jqf 'd.get("txid","")')
[ "${#txid1}" = 64 ] || { fail "splicein txid: $first"; exit 2; }
for _ in $(seq 1 40); do
  in_mempool "$txid1" && break
  sleep 3
done
in_mempool "$txid1" || { fail "splicein tx $txid1 not in mempool"; exit 2; }
row splicein "txid=$txid1"

second=$(rpc "$(API sa)" splicein "{\"node_id\":\"${ID[sb]}\",\"amount\":$SPLICE_IN_SATS}")
msg=$(fundchannel_error "$second")
echo "$msg" | grep -q "bumpchannelfee" || { fail "second splicein: $msg"; exit 2; }
row splicein-pending "ok"

bumped=$(TMO=150 rpc "$(API sa)" bumpchannelfee "{\"node_id\":\"${ID[sb]}\",\"sat_per_vbyte\":20}")
ferr=$(fundchannel_error "$bumped")
[ -z "$ferr" ] || { fail "bumpchannelfee: $ferr"; exit 2; }
txid2=$(echo "$bumped" | jqf 'd.get("txid","")')
[ "${#txid2}" = 64 ] || { fail "bump txid: $bumped"; exit 2; }
[ "$txid2" != "$txid1" ] || { fail "bump reused txid $txid1"; exit 2; }
for _ in $(seq 1 40); do
  in_mempool "$txid2" && break
  sleep 3
done
in_mempool "$txid2" || { fail "bump tx $txid2 not in mempool"; exit 2; }
row bump "txid=$txid2"
advance 8
wait_wallet_synced 300 || true

want_in=$(( CHANNEL_SATS + SPLICE_IN_SATS ))
deadline=$(( $(date +%s) + CONFIRM_WAIT ))
after_in=0
while :; do
  after_in=$(rpc "$(API sa)" channels | jqf 'd["channels"][0]["amount"]')
  [ "${after_in:-0}" -ge "$want_in" ] && break
  [ "$(date +%s)" -gt "$deadline" ] && { fail "channel value $after_in after splice-in, wanted >= $want_in"; exit 2; }
  sleep 10
done
row splicein-locked "amount=$after_in"

addr=$(rpc "$(API sa)" new_addr | jqf 'd["address"]')
[ -n "$addr" ] || { fail "no splice-out address"; exit 2; }
out=$(TMO=150 rpc "$(API sa)" spliceout "{\"node_id\":\"${ID[sb]}\",\"amount\":$SPLICE_OUT_SATS,\"address\":\"$addr\"}")
ferr=$(fundchannel_error "$out")
[ -z "$ferr" ] || { fail "spliceout: $ferr"; exit 2; }
txid3=$(echo "$out" | jqf 'd.get("txid","")')
[ "${#txid3}" = 64 ] || { fail "spliceout txid: $out"; exit 2; }
for _ in $(seq 1 40); do
  in_mempool "$txid3" && break
  sleep 3
done
in_mempool "$txid3" || { fail "spliceout tx $txid3 not in mempool"; exit 2; }
row spliceout "txid=$txid3"
advance 8
wait_wallet_synced 300 || true

deadline=$(( $(date +%s) + CONFIRM_WAIT ))
after_out=0
while :; do
  after_out=$(rpc "$(API sa)" channels | jqf 'd["channels"][0]["amount"]')
  [ "${after_out:-0}" -gt 0 ] && [ $(( after_out + SPLICE_OUT_SATS )) -le "$after_in" ] && break
  [ "$(date +%s)" -gt "$deadline" ] && { fail "channel value $after_out after splice-out, before=$after_in"; exit 2; }
  sleep 10
done
row spliceout-locked "amount=$after_out"

inv=$(TMO=30 rpc "$(API sb)" invoice "{\"amount_msat\":10000,\"description\":\"splice-qa\"}" | jqf 'd.get("bolt11","")')
[ -n "$inv" ] || { fail "sb issued no invoice"; exit 2; }
pay=$(TMO=120 rpc "$(API sa)" pay "{\"invoice_str\":\"$inv\"}")
state=$(echo "$pay" | jqf 'd.get("state","")')
pre=$(echo "$pay" | jqf 'd.get("payment_preimage") or ""')
if [ "$state" != Success ] || [ -z "$pre" ]; then
  fail "pay after splice state=${state:-none} body=$(echo "$pay" | head -c 200)"
  exit 2
fi
row pay "state=$state preimage=${pre:0:16}"

health_scan_since "0 0" || { fail "panic in node log"; exit 2; }
cleanup_nodes
say "SPLICE QA COMPLETE: splice-in $txid1 bump $txid2 splice-out $txid3 pay ok"
exit 0
