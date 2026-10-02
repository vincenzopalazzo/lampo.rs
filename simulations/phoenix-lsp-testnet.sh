#!/usr/bin/env bash
# Testnet3 interop: lampo as a client of ACINQ's Phoenix LSP, paying and
# being paid by a phoenixd node through that LSP.
#
#   lampo  --(channel)--  ACINQ LSP  --(on-the-fly channel)--  phoenixd
#
# No chain sync: lampo's chain backend is the folgore lampo plugin against
# mempool.space, phoenixd uses its default electrum server. The script
# starts phoenixd and lampo, waits for lampo to be funded (it prints the
# address; send >= FUND_SAT tsat to it from any testnet3 wallet), opens a
# channel to the LSP (which demands at least 400k sat), pays a phoenixd
# invoice through the LSP, then has phoenixd pay a lampo invoice back.
# Testnet3 only: never point it at mainnet. Never delete `lampod.pid`.
#
# Required:
#   BIN       lampod-cli built from this branch
#   CLI       lampo-cli built from this branch
#   FOLGORE   folgore-lampo (coffee-tools/folgore PR 105)
#   PHX       directory holding phoenixd and phoenix-cli (0.9.x)
#
# Optional:
#   SIMDIR          default $PWD/sim-run-phoenix-lsp
#   MEMPOOL_URL     default https://mempool.space/testnet/api
#   LAMPO_PORT      default 19735, API_PORT default 19736
#   PHX_PORT        default 9740
#   CHANNEL_SAT     default 450000 (LSP minimum is 400000)
#   FUND_SAT        default 600000 (channel + fees + anchor reserve)
#   PAY_OUT_SAT     default 60000 (lampo -> phoenixd; must cover phoenixd's
#                   liquidity fee, ~22k sat for 2m of liquidity)
#   PAY_IN_SAT      default 20000 (phoenixd -> lampo)
set -euo pipefail

BIN=${BIN:?set BIN to lampod-cli}
CLI=${CLI:?set CLI to lampo-cli}
FOLGORE=${FOLGORE:?set FOLGORE to the folgore-lampo plugin binary}
PHX=${PHX:?set PHX to the phoenixd release directory}
SIMDIR=${SIMDIR:-$PWD/sim-run-phoenix-lsp}
MEMPOOL_URL=${MEMPOOL_URL:-https://mempool.space/testnet/api}
LAMPO_PORT=${LAMPO_PORT:-19735}
API_PORT=${API_PORT:-19736}
PHX_PORT=${PHX_PORT:-9740}
CHANNEL_SAT=${CHANNEL_SAT:-450000}
FUND_SAT=${FUND_SAT:-600000}
PAY_OUT_SAT=${PAY_OUT_SAT:-60000}
PAY_IN_SAT=${PAY_IN_SAT:-20000}
LSP_ID=03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134

LAMPO_DIR=$SIMDIR/lampo
PHX_DIR=$SIMDIR/phoenix
mkdir -p "$LAMPO_DIR/testnet" "$PHX_DIR"

lcli() { "$CLI" -u "http://127.0.0.1:$API_PORT" "$@"; }
pcli() {
  local pw
  pw=$(grep -o "http-password=.*" "$PHX_DIR/phoenix.conf" | head -1 | cut -d= -f2)
  echo "$pw" | PHOENIX_DATADIR="$PHX_DIR" "$PHX/phoenix-cli" --http-bind-port="$PHX_PORT" \
    --http-password-file=/dev/stdin "$@"
}
tip() { curl -sS -m 15 "$MEMPOOL_URL/blocks/tip/height"; }
# The helpers are exported so `wait_for` can run them through `bash -c`.
export -f lcli pcli
export CLI API_PORT PHX PHX_DIR PHX_PORT
wait_for() { # wait_for <seconds> <description> <bash -c snippet>
  local deadline=$(( $(date +%s) + $1 )) what=$2 snippet=$3
  until bash -c "$snippet" >/dev/null 2>&1; do
    if [ "$(date +%s)" -ge "$deadline" ]; then echo "timeout waiting for $what" >&2; return 1; fi
    sleep 10
  done
}

cleanup() {
  [ -f "$PHX_DIR/phoenixd.pid" ] && kill "$(cat "$PHX_DIR/phoenixd.pid")" 2>/dev/null || true
  [ -f "$LAMPO_DIR/lampod.pid" ] && kill "$(cat "$LAMPO_DIR/lampod.pid")" 2>/dev/null || true
}
trap cleanup EXIT

echo "== phoenixd (testnet3, auto-liquidity 2m)"
if ! [ -f "$PHX_DIR/phoenixd.pid" ] || ! kill -0 "$(cat "$PHX_DIR/phoenixd.pid")" 2>/dev/null; then
  PHOENIX_DATADIR="$PHX_DIR" nohup "$PHX/phoenixd" --chain=testnet --auto-liquidity=2m \
    --http-bind-port="$PHX_PORT" > "$PHX_DIR/console.log" 2>&1 < /dev/null &
  echo $! > "$PHX_DIR/phoenixd.pid"
fi
wait_for 120 "phoenixd" "pcli getinfo" || { tail -20 "$PHX_DIR/console.log"; exit 1; }
pcli getinfo

echo "== lampo (folgore plugin on $MEMPOOL_URL, phoenix-lsp=default)"
if [ ! -f "$LAMPO_DIR/testnet/lampo.conf" ]; then
  cat > "$LAMPO_DIR/testnet/lampo.conf" <<EOF
network=testnet
port=$LAMPO_PORT
bind-addr=127.0.0.1
api-host=http://127.0.0.1
api-port=$API_PORT
# Chain RPC goes through the plugin; the core URL is only validated.
backend=core
core-url=http://127.0.0.1:1
core-user=unused
core-pass=unused
log-level=debug
log-file=$LAMPO_DIR/testnet/lampod.log
reindex=$(tip)
phoenix-lsp=default
phoenix-auto-liquidity=2000000
phoenix-max-fee-credit=50000
phoenix-max-mining-fee=20000
EOF
fi
start_lampo() {
  local restore=()
  [ -f "$LAMPO_DIR/testnet/wallet.dat" ] && restore=(--restore-wallet)
  setsid nohup "$BIN" --data-dir "$LAMPO_DIR" --network testnet "${restore[@]}" \
    --plugin "$FOLGORE -- --mempool-space-url $MEMPOOL_URL" \
    >> "$LAMPO_DIR/console.log" 2>&1 < /dev/null &
}
# A first start only creates the wallet and exits; the second one runs.
if [ ! -f "$LAMPO_DIR/testnet/wallet.dat" ]; then
  start_lampo; sleep 5
fi
start_lampo
wait_for 120 "lampo api" "lcli getinfo"
wait_for 120 "LSP handshake" "lcli phoenixlsp-info | grep -q '\"connected\": true'"
wait_for 120 "recommended_feerates" "lcli phoenixlsp-info | grep -q funding_feerate"
lcli phoenixlsp-info | grep -E "connected|funding_feerate\"|on_the_fly_funding|funding_fee_credit"

echo "== funding"
funds_sat() { lcli funds | grep -o '"amount_msat": [0-9]*' | awk '{s+=$2} END {print int(s/1000)}'; }
if [ "$(lcli channels | grep -c '"ready": true')" -eq 0 ] && [ "$(funds_sat)" -lt "$FUND_SAT" ]; then
  echo "send at least $FUND_SAT tsat to: $(lcli new_addr | grep -o 'tb1[a-z0-9]*')"
  wait_for 7200 "funds" "[ \$(lcli funds | grep -o '\"amount_msat\": [0-9]*' | awk '{s+=\$2} END {print int(s/1000)}') -ge $FUND_SAT ]"
fi
lcli funds

echo "== channel to the LSP"
if [ "$(lcli channels | grep -c "$LSP_ID")" -eq 0 ]; then
  lcli fundchannel --node_id "$LSP_ID" --amount "$CHANNEL_SAT" --public false | head -5
fi
wait_for 7200 "channel ready" "lcli channels | grep -q '\"ready\": true'"
lcli channels | grep -E "ready|available"

echo "== lampo -> LSP -> phoenixd ($PAY_OUT_SAT sat)"
inv=$(pcli createinvoice --amountSat "$PAY_OUT_SAT" --description "lampo to phoenix" | grep -o '"serialized": *"[^"]*"' | cut -d'"' -f4)
lcli pay --invoice_str "$inv" | grep -E "state|status|payment_hash|preimage" || true
wait_for 120 "phoenixd balance" "pcli getbalance | grep -q '\"balanceSat\": [1-9]'"
pcli getbalance

echo "== phoenixd -> LSP -> lampo ($PAY_IN_SAT sat)"
recv_before=$(lcli channels | grep -o '"available_balance_for_send_msat": [0-9]*' | head -1 | awk '{print $2}')
linv=$(lcli invoice --amount_msat $((PAY_IN_SAT * 1000)) --description "phoenix to lampo" | grep -o 'lntb[a-z0-9]*' | head -1)
pcli payinvoice --invoice "$linv"
echo
wait_for 120 "lampo balance" "[ \$(lcli channels | grep -o '\"available_balance_for_send_msat\": [0-9]*' | head -1 | awk '{print \$2}') -gt $recv_before ]"
lcli channels | grep -E "available"

echo "PHOENIX LSP INTEROP COMPLETE: paid $PAY_OUT_SAT sat out and received $PAY_IN_SAT sat through $LSP_ID"
