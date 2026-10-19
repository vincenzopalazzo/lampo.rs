#!/usr/bin/env bash
#
# simulations/splice.sh — two-node splice QA.
#
# sa opens a channel to sb, splices in, refuses a second splice while one
# is pending, bumps the fee, splices out, then pays across the channel.
# Asserts RPC fields and the channel value, never log prose.
#
# Ports API 8411-8412 / P2P 20411-20412. Data dir defaults to
# $HOME/lampo-splice-qa, not the soak tree under $HOME/lampo-sim.
#
# Env: REPO BIN SIMDIR SEED SPLICE_IN_SATS(50000) SPLICE_OUT_SATS(10000)
#      CHANNEL_SATS(400000)
#
# Usage:
#   REPO=$HOME/lampo-splice-qa BIN=$REPO/target/release/lampod-cli \
#     ./simulations/splice.sh
set -uo pipefail

REPO=${REPO:-$HOME/lampo-splice-qa}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$HOME/lampo-splice-qa/sim-run}
API_BASE=${API_BASE:-8410}
P2P_BASE=${P2P_BASE:-20410}
CHANNEL_SATS=${CHANNEL_SATS:-400000}
SPLICE_IN_SATS=${SPLICE_IN_SATS:-50000}
SPLICE_OUT_SATS=${SPLICE_OUT_SATS:-10000}
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

say "splice QA: bin=$BIN simdir=$SIMDIR seed=$SEED"
[ -x "$BIN" ] || { say "binary missing: $BIN"; exit 1; }
bcli getblockchaininfo | jqf 'd["result"]["chain"]' | grep -q regtest \
  || { say "bitcoind at $CORE_URL is not regtest"; exit 1; }

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

say "starting sa and sb"
for n in "${ALLNODES[@]}"; do
  start_node "$n"
  ID[$n]=$(wait_up "$n") || { fail "$n never came up"; exit 2; }
  say "  $n = ${ID[$n]:0:16}.. api :$(API "$n") p2p :$(P2P "$n")"
done
for n in "${ALLNODES[@]}"; do fund_node "$n" 0.05 || { fail "fund $n"; exit 2; }; done
sleep 140
wait_wallet_synced 420 || { fail "wallets never synced"; exit 2; }

open_channel sa sb "${ID[sb]}" "$CHANNEL_SATS" 0 || { fail "open sa->sb"; exit 2; }
deadline=$(( $(date +%s) + 240 ))
while :; do
  [ "$(ready_channels sa)" -ge 1 ] && [ "$(ready_channels sb)" -ge 1 ] && break
  [ "$(date +%s)" -gt "$deadline" ] && { fail "channel never ready"; exit 2; }
  mine 2
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
for _ in $(seq 1 20); do
  bcli getrawmempool | grep -q "$txid1" && break
  sleep 3
done
bcli getrawmempool | grep -q "$txid1" || { fail "splicein tx $txid1 not in mempool"; exit 2; }
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
for _ in $(seq 1 20); do
  bcli getrawmempool | grep -q "$txid2" && break
  sleep 3
done
bcli getrawmempool | grep -q "$txid2" || { fail "bump tx $txid2 not in mempool"; exit 2; }
row bump "txid=$txid2"
mine 8
wait_wallet_synced 300 || true

want_in=$(( CHANNEL_SATS + SPLICE_IN_SATS ))
deadline=$(( $(date +%s) + 300 ))
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
for _ in $(seq 1 20); do
  bcli getrawmempool | grep -q "$txid3" && break
  sleep 3
done
bcli getrawmempool | grep -q "$txid3" || { fail "spliceout tx $txid3 not in mempool"; exit 2; }
row spliceout "txid=$txid3"
mine 8
wait_wallet_synced 300 || true

deadline=$(( $(date +%s) + 300 ))
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
