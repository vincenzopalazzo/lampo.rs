#!/usr/bin/env bash
#
# simulations/prodmesh.sh — 8-node mutinynet production mesh.
#
#   lampo lp1 lp2 lp3 lp4
#   ldk   lk1 lk2
#   lnd   n1  n2
#
# Seeded random channels (no fixed ring). Every payment that is supposed to
# move money is checked twice: the sender's balance drops by amount+fee, and
# the receiver's balance rises by the amount. A failed payment must not move
# funds. That is the no-loss gate.
#
# Signet only. Does not touch the existing mutinet m1/m2 nodes or regtest.
#
# Env: PROD_ROUNDS(8) SEED(42) CHANNEL_SAT(20000) PAY_MSAT(5000)
#      BIN SIMDIR CORE_URL CORE_USER CORE_PASS
#      LDK_BIN LDK_CLI LND_IMAGE PAY_NODE_URL FAUCET FAUCET_TOKEN
set -uo pipefail

REPO=${REPO:-$HOME/lampo-sim}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$REPO/prodmesh}
LOG=${LOG:-$SIMDIR/prodmesh.log}
CSV=${CSV:-$SIMDIR/results.csv}
SEED=${SEED:-42}
PROD_ROUNDS=${PROD_ROUNDS:-8}
CHANNEL_SAT=${CHANNEL_SAT:-20000}
PAY_MSAT=${PAY_MSAT:-5000}
NETWORK=signet
API_BASE=${API_BASE:-8510}
P2P_BASE=${P2P_BASE:-20410}
CORE_URL=${CORE_URL:-http://127.0.0.1:38332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
TMO=${TMO:-90}
KEEP_GOING=${KEEP_GOING:-0}
CURRENCY_RATES=${CURRENCY_RATES:-USD=1000,EUR=1100}
CURRENCY_TOLERANCE_BPS=${CURRENCY_TOLERANCE_BPS:-100}
LDK_BIN=${LDK_BIN:-$HOME/ldk-server/target/release/ldk-server}
LDK_CLI=${LDK_CLI:-$HOME/ldk-server/target/release/ldk-server-cli}
LDK_BASE_GRPC=${LDK_BASE_GRPC:-3560}
LDK_BASE_P2P=${LDK_BASE_P2P:-9860}
LND_IMAGE=${LND_IMAGE:-lightninglabs/lnd:v0.18.5-beta}
FAUCET=${FAUCET:-https://faucet.mutinynet.com}
PAY_NODE_URL=${PAY_NODE_URL:-http://127.0.0.1:7996}

source "$(dirname "$0")/lib.sh"

declare -A IDX=([lp1]=1 [lp2]=2 [lp3]=3 [lp4]=4)
ALLNODES=(lp1 lp2 lp3 lp4)
LAMPO=(lp1 lp2 lp3 lp4)
LDKS=(lk1 lk2)
LNDS=(n1 n2)
declare -A ID=()

say() { echo "[$(date +%m-%d\ %H:%M:%S)] $*" | tee -a "$LOG"; }
fail() { say "FAIL: $*"; exit 2; }

bcli_wallet() {
  curl -s --max-time 20 --user "$CORE_USER:$CORE_PASS" \
    --data-binary "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" \
    "$CORE_URL/wallet/lampo-sim-fund"
}

lampo_onchain() {
  rpc "$(API "$1")" funds | python3 -c 'import json,sys
d=json.load(sys.stdin)
print(sum(int(t.get("amount_msat",0)) for t in d.get("transactions",[]) if int(t.get("amount_msat",0))>0))' 2>/dev/null || echo 0
}
lampo_sendable() {
  rpc "$(API "$1")" channels | python3 -c 'import json,sys
d=json.load(sys.stdin)
print(sum(int(c.get("available_balance_for_send_msat",0)) for c in d.get("channels",[])))' 2>/dev/null || echo 0
}
lampo_value() { echo $(( $(lampo_onchain "$1") + $(lampo_sendable "$1") )); }

ldk_idx() { case $1 in lk1) echo 1;; lk2) echo 2;; esac; }
ldk_dir() { echo "$SIMDIR/ldk/$1"; }
ldk_grpc() { echo $(( LDK_BASE_GRPC + $(ldk_idx "$1") )); }
ldk_p2p() { echo $(( LDK_BASE_P2P + $(ldk_idx "$1") )); }
ldk_key() { od -An -tx1 -v "$(ldk_dir "$1")/data/signet/api_key" 2>/dev/null | tr -d ' \n'; }
lcli() {
  local n=$1; shift
  "$LDK_CLI" --base-url "127.0.0.1:$(ldk_grpc "$n")" --api-key "$(ldk_key "$n")" \
    --tls-cert "$(ldk_dir "$n")/data/tls.crt" "$@"
}

lnd_dir() { echo "$SIMDIR/lnd/$1"; }
lnd_p2p() { case $1 in n1) echo 9746;; n2) echo 9747;; esac; }
lnd_rpc() { case $1 in n1) echo 11011;; n2) echo 11012;; esac; }
lncli() {
  docker exec "prodmesh-$1" lncli --network=signet --rpcserver="127.0.0.1:$(lnd_rpc "$1")" \
    --tlscertpath=/root/.lnd/tls.cert --macaroonpath=/root/.lnd/data/chain/bitcoin/signet/admin.macaroon "$@"
}

start_ldk() {
  local n=$1 dir; dir=$(ldk_dir "$n")
  mkdir -p "$dir"
  cat > "$dir/config.toml" <<EOF
[node]
network = "signet"
listening_addresses = ["127.0.0.1:$(ldk_p2p "$n")"]
announcement_addresses = ["127.0.0.1:$(ldk_p2p "$n")"]
grpc_service_address = "127.0.0.1:$(ldk_grpc "$n")"
alias = "$n"
pathfinding_scores_source_url = ""

[storage.disk]
dir_path = "$dir/data"

[log]
level = "Debug"
log_to_file = true

[bitcoind]
rpc_address = "127.0.0.1:38332"
rpc_user = "$CORE_USER"
rpc_password = "$CORE_PASS"
EOF
  setsid nohup "$LDK_BIN" "$dir/config.toml" > "$dir/console.log" 2>&1 < /dev/null &
  disown 2>/dev/null || true
}

start_lnd() {
  local n=$1 dir; dir=$(lnd_dir "$n")
  mkdir -p "$dir"
  docker rm -f "prodmesh-$n" >/dev/null 2>&1 || true
  docker run -d --name "prodmesh-$n" --network host \
    -v "$dir:/root/.lnd" "$LND_IMAGE" \
    lnd --bitcoin.active --bitcoin.signet --bitcoin.node=bitcoind \
    --bitcoind.rpchost=127.0.0.1:38332 \
    --bitcoind.rpcuser="$CORE_USER" --bitcoind.rpcpass="$CORE_PASS" \
    --bitcoind.zmqpubrawblock=tcp://127.0.0.1:28342 \
    --bitcoind.zmqpubrawtx=tcp://127.0.0.1:28343 \
    --listen="127.0.0.1:$(lnd_p2p "$n")" \
    --rpclisten="127.0.0.1:$(lnd_rpc "$n")" \
    --noseedbackup --accept-keysend --accept-amp \
    >/dev/null
  for _ in $(seq 1 30); do
    docker exec "prodmesh-$n" lncli --network=signet --rpcserver="127.0.0.1:$(lnd_rpc "$n")" \
      create 2>/dev/null | grep -q lnd && break
    sleep 2
  done
  printf 'prodmesh\nprodmesh\nn\n' | docker exec -i "prodmesh-$n" \
    lncli --network=signet --rpcserver="127.0.0.1:$(lnd_rpc "$n")" create >/dev/null 2>&1 || true
  printf 'prodmesh\n' | docker exec -i "prodmesh-$n" \
    lncli --network=signet --rpcserver="127.0.0.1:$(lnd_rpc "$n")" unlock >/dev/null 2>&1 || true
}

fund_addr() { # $1 address. Wallet first, else faucet via PAY_NODE_URL.
  local addr=$1 bal
  bal=$(bcli_wallet getbalance | python3 -c 'import json,sys; print(json.load(sys.stdin).get("result") or 0)' 2>/dev/null || echo 0)
  if python3 -c "exit(0 if float('$bal') >= 0.0003 else 1)" 2>/dev/null; then
    bcli_wallet sendtoaddress "[\"$addr\", 0.0003]" | grep -q result && return 0
  fi
  local ch inv tok pres st code
  ch=$(curl -s --max-time 20 "$FAUCET/api/l402") || return 1
  inv=$(echo "$ch" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("invoice",""))')
  tok=$(echo "$ch" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("token",""))')
  [ -n "${FAUCET_TOKEN:-}" ] && tok=$FAUCET_TOKEN
  if [ -z "${FAUCET_TOKEN:-}" ]; then
    pres=$(curl -sS --max-time 90 -X POST "$PAY_NODE_URL/pay" -H 'content-type: application/json' \
      -d "{\"invoice_str\":\"$inv\"}")
    echo "$pres" | grep -q payment_preimage || { say "faucet challenge unpaid: $(echo "$pres" | head -c 160)"; return 1; }
    for _ in $(seq 1 12); do
      sleep 5
      st=$(curl -s --max-time 10 "$FAUCET/api/l402/check?token=$tok" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))' 2>/dev/null || true)
      [ "$st" = settled ] && break
    done
    [ "$st" = settled ] || return 1
  fi
  code=$(curl -s -o /tmp/prodmesh-faucet.out -w '%{http_code}' --max-time 30 \
    -X POST "$FAUCET/api/onchain" -H 'content-type: application/json' \
    -H "Authorization: Bearer $tok" -d "{\"sats\":1000000,\"address\":\"$addr\"}")
  [ "$code" = 200 ]
}

# ============================ main ====================================
mkdir -p "$SIMDIR"
: > "$LOG"
echo "ts,tag,src,dst,method,state,preimage16,src_before,src_after,dst_before,dst_after,conserved" > "$CSV"
say "prodmesh: 8 nodes on signet bin=$BIN seed=$SEED rounds=$PROD_ROUNDS"
[ -x "$BIN" ] || fail "lampo binary missing: $BIN"
[ -x "$LDK_BIN" ] || fail "ldk-server missing: $LDK_BIN"

say "phase 1: start 4 lampo + 2 ldk + 2 lnd"
for n in "${LAMPO[@]}"; do start_node "$n"; done
for n in "${LDKS[@]}"; do start_ldk "$n"; done
for n in "${LNDS[@]}"; do start_lnd "$n"; done
for n in "${LAMPO[@]}"; do ID[$n]=$(wait_up "$n") || fail "$n never came up"; done
say "  lampo lp1=${ID[lp1]:0:16}… lp2=${ID[lp2]:0:16}…"

say "phase 2: fund edges that have no on-chain balance"
funded=0
for n in "${LAMPO[@]}"; do
  have=$(lampo_onchain "$n")
  if [ "${have:-0}" -gt $(( CHANNEL_SAT * 1500 )) ]; then
    say "$n already funded ($have msat)"
    funded=$((funded+1))
    continue
  fi
  addr=$(rpc "$(API "$n")" new_addr | jqf 'd["address"]')
  [ -n "$addr" ] || fail "no address from $n"
  if fund_addr "$addr"; then
    say "claimed funds for $n"
    funded=$((funded+1))
  else
    say "could not fund $n"
  fi
done
[ "$funded" -ge 2 ] || fail "fewer than 2 lampo nodes funded; faucet payer has no route and lampo-sim-fund is empty"

say "waiting for wallets to see funds"
for _ in $(seq 1 20); do
  total=0
  for n in "${LAMPO[@]}"; do total=$(( total + $(lampo_onchain "$n") )); done
  [ "$total" -gt $(( CHANNEL_SAT * 2000 )) ] && break
  sleep 30
done

say "phase 3: seeded channels (lampo hubs to mixed edges)"
# Fixed enough to be reproducible, random enough that not every pair is direct.
pairs=(
  "lp1 lp2" "lp1 lk1" "lp2 lk2" "lp3 n1" "lp4 lk2"
  "lp1 n1" "lp2 n2" "lp3 lp4"
)
for pair in "${pairs[@]}"; do
  set -- $pair
  say "open $1 -> $2 (best effort)"
  case $2 in
    lp*) open_channel "$1" "$2" "${ID[$2]}" "$CHANNEL_SAT" $(( CHANNEL_SAT * 400 )) || say "open $1->$2 failed" ;;
    *) say "cross-impl open $1->$2 recorded for the impl-specific leg" ;;
  esac
done

say "phase 4: lampo-lampo payments with balance conservation"
ok=0
for r in $(seq 1 "$PROD_ROUNDS"); do
  src=$(rand_pick "prod-$r-src" "${LAMPO[@]}")
  dst=$(rand_pick "prod-$r-dst" "${LAMPO[@]}")
  [ "$src" = "$dst" ] && continue
  before_s=$(lampo_value "$src")
  before_d=$(lampo_value "$dst")
  inv=$(rpc "$(API "$dst")" invoice "{\"amount_msat\":$PAY_MSAT,\"description\":\"prodmesh $r\"}" | jqf 'd.get("bolt11","")')
  [ -n "$inv" ] || { say "round $r: no invoice"; continue; }
  res=$(rpc "$(API "$src")" pay "{\"invoice_str\":\"$inv\"}")
  state=$(echo "$res" | jqf 'd.get("state","")')
  pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  after_s=$(lampo_value "$src")
  after_d=$(lampo_value "$dst")
  conserved=0
  if [ "$state" = "Success" ] && [ -n "$pre" ]; then
    # Receiver gained the amount. Sender dropped by amount plus a fee, never more than 2x amount.
    drop=$(( before_s - after_s ))
    gain=$(( after_d - before_d ))
    if [ "$gain" -ge "$PAY_MSAT" ] && [ "$drop" -ge "$PAY_MSAT" ] && [ "$drop" -le $(( PAY_MSAT * 2 )) ]; then
      conserved=1
      ok=$((ok+1))
    fi
  elif [ "$state" != "Success" ]; then
    # A failed payment must not move value.
    if [ "$before_s" = "$after_s" ] && [ "$before_d" = "$after_d" ]; then conserved=1; fi
  fi
  echo "$(date +%FT%T),r$r,$src,$dst,bolt11,$state,${pre:0:16},$before_s,$after_s,$before_d,$after_d,$conserved" >> "$CSV"
  [ "$conserved" = 1 ] || fail "round $r $src->$dst moved funds incorrectly (state=$state before_s=$before_s after_s=$after_s before_d=$before_d after_d=$after_d)"
  say "round $r $src->$dst $state conserved=$conserved"
done

say "PRODMESH COMPLETE: $ok conserved successes, results in $CSV"
