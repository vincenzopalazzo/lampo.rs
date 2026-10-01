#!/usr/bin/env bash
# Testnet3 BLIP-42 contact interop: lampo (feat/blip42-contacts) <-> phoenixd.
#
# Phoenix only speaks testnet3 (`--chain=testnet` maps to Chain.Testnet3).
# Lampo needs a bitcoind on that same chain. This script does not mine, does
# not touch regtest sim-run nodes, and does not delete lampod.pid.
#
# Required:
#   BIN          path to lampod-cli built from feat/blip42-contacts
#   PHX          path to phoenixd built from vincenzopalazzo/phoenixd@blip42-contacts
#   CORE_URL     http://127.0.0.1:18333
#   CORE_USER / CORE_PASS
#
# Optional:
#   SIMDIR       default $PWD/sim-run-phoenix-contacts
#   PHX_DIR      default $SIMDIR/phoenix
#   PHX_PORT     default 9741
#   PHX_PASS     default interop-testnet3
#   LAMPO_PORT   default 19735
#   API_PORT     default 19736
#   AMOUNT_SAT   default 1000 (phoenix payinvoice / paycontact unit is sats)
set -euo pipefail

REPO=${REPO:-$PWD}
BIN=${BIN:?set BIN to target/release/lampod-cli}
PHX=${PHX:?set PHX to the phoenixd binary}
SIMDIR=${SIMDIR:-$REPO/sim-run-phoenix-contacts}
PHX_DIR=${PHX_DIR:-$SIMDIR/phoenix}
PHX_PORT=${PHX_PORT:-9741}
PHX_PASS=${PHX_PASS:-interop-testnet3}
LAMPO_PORT=${LAMPO_PORT:-19735}
API_PORT=${API_PORT:-19736}
CORE_URL=${CORE_URL:-http://127.0.0.1:18333}
CORE_USER=${CORE_USER:-testnet3}
CORE_PASS=${CORE_PASS:-testnet3pass}
AMOUNT_SAT=${AMOUNT_SAT:-1000}
LAMPO_DATA=$SIMDIR/lampo

mkdir -p "$SIMDIR" "$PHX_DIR" "$LAMPO_DATA"

btc() {
  bitcoin-cli -rpcconnect=127.0.0.1 -rpcport="${CORE_URL##*:}" \
    -rpcuser="$CORE_USER" -rpcpassword="$CORE_PASS" "$@"
}

wait_bitcoind() {
  echo "waiting for testnet3 bitcoind to leave IBD..."
  local i chain blocks headers ibd
  for i in $(seq 1 720); do
    if chain=$(btc getblockchaininfo 2>/dev/null); then
      python3 - << PY
import json,sys
d=json.loads('''$chain''')
print(d["chain"], d["blocks"], d["headers"], d["initialblockdownload"])
open("/tmp/lampo-t3-sync","w").write(f"{d['chain']} {d['blocks']} {d['headers']} {d['initialblockdownload']}")
sys.exit(0 if d["chain"]=="test" and not d["initialblockdownload"] and d["blocks"]>5000000 else 1)
PY
      if [[ $? -eq 0 ]]; then
        echo "bitcoind synced: $(cat /tmp/lampo-t3-sync)"
        return 0
      fi
    fi
    sleep 30
  done
  echo "bitcoind still in IBD after 6h: $(cat /tmp/lampo-t3-sync 2>/dev/null)" >&2
  return 1
}

lampo_call() {
  local method=$1 body=$2
  curl -fsS -m 180 -H 'content-type: application/json' \
    -d "$body" "http://127.0.0.1:${API_PORT}/${method}"
}

phx_call() {
  local method=$1
  shift
  curl -fsS -m 180 -u ":${PHX_PASS}" -X POST \
    "http://127.0.0.1:${PHX_PORT}/${method}" "$@"
}

start_phoenix() {
  if curl -fsS -m 5 -u ":${PHX_PASS}" "http://127.0.0.1:${PHX_PORT}/getinfo" >/dev/null 2>&1; then
    echo "phoenixd already up"
    return 0
  fi
  echo "starting phoenixd --chain=testnet"
  "$PHX" --chain=testnet \
    --http-bind-port="$PHX_PORT" \
    --http-password="$PHX_PASS" \
    --seed-path="$PHX_DIR/seed.dat" \
    --agree-to-terms-of-service \
    >"$PHX_DIR/phoenix.log" 2>&1 &
  echo $! >"$PHX_DIR/phoenix.pid"
  local i
  for i in $(seq 1 30); do
    curl -fsS -m 5 -u ":${PHX_PASS}" "http://127.0.0.1:${PHX_PORT}/getinfo" \
      && echo && return 0
    sleep 2
  done
  echo "phoenixd did not come up" >&2
  tail -40 "$PHX_DIR/phoenix.log" >&2 || true
  return 1
}

start_lampo() {
  if [[ ! -f "$LAMPO_DATA/testnet/lampo.conf" ]]; then
    "$BIN" --network testnet --data-dir "$LAMPO_DATA" new-wallet \
      >"$SIMDIR/lampo-mnemonic.txt"
    cat >"$LAMPO_DATA/testnet/lampo.conf" << CONF
network=testnet
port=${LAMPO_PORT}
announce-addr=127.0.0.1
api-host=http://127.0.0.1
api-port=${API_PORT}
backend=core
core-url=${CORE_URL}
core-user=${CORE_USER}
core-pass=${CORE_PASS}
log-level=debug
log-file=${LAMPO_DATA}/testnet/lampo.log
CONF
  fi
  if curl -fsS -m 5 "http://127.0.0.1:${API_PORT}/getinfo" >/dev/null 2>&1; then
    echo "lampo already up"
    return 0
  fi
  echo "starting lampod-cli on testnet"
  "$BIN" --network testnet --data-dir "$LAMPO_DATA" \
    >"$SIMDIR/lampo-console.log" 2>&1 &
  local i
  for i in $(seq 1 60); do
    curl -fsS -m 5 "http://127.0.0.1:${API_PORT}/getinfo" && echo && return 0
    sleep 2
  done
  echo "lampo did not come up" >&2
  tail -40 "$SIMDIR/lampo-console.log" >&2 || true
  return 1
}

echo "=== phoenix-contacts testnet3 interop ==="
wait_bitcoind
start_phoenix
start_lampo

echo "=== node info ==="
lampo_call getinfo '{}' | tee "$SIMDIR/lampo-getinfo.json"
curl -fsS -u ":${PHX_PASS}" "http://127.0.0.1:${PHX_PORT}/getinfo" | tee "$SIMDIR/phoenix-getinfo.json"
echo

# Phoenix offer is created at startup and printed in the log. Prefer the API
# if present, otherwise the first lno1 in the log / getinfo.
PHOENIX_OFFER=$(curl -fsS -m 20 -u ":${PHX_PASS}" -X POST \
  "http://127.0.0.1:${PHX_PORT}/createoffer" -d 'description=lampo-interop' \
  | python3 -c 'import sys,json,re
raw=sys.stdin.read()
try:
    d=json.loads(raw)
    print(d.get("offer") or d.get("serialized") or "")
except Exception:
    m=re.search(r"lno1[a-z0-9]+", raw)
    print(m.group(0) if m else "")
')
if [[ -z "$PHOENIX_OFFER" ]]; then
  PHOENIX_OFFER=$(grep -oE 'lno1[a-z0-9]+' "$PHX_DIR/phoenix.log" | head -1 || true)
fi
echo "phoenix offer: ${PHOENIX_OFFER:0:80}..."
[[ -n "$PHOENIX_OFFER" ]]

LAMPO_NODE=$(python3 -c 'import json; print(json.load(open("'"$SIMDIR/lampo-getinfo.json"'"))["node_id"])')
echo "lampo node: $LAMPO_NODE"

# Phoenix testnet3 trampoline. Compact payer offers must be introduced by a
# public peer lampo has a channel with, not by lampo itself.
LSP_ID=03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134
LSP_HOST=13.248.222.197
LSP_PORT=9735
echo "=== connect lampo to the ACINQ testnet3 LSP ==="
lampo_call connect "$(printf '%s' "{\"node_id\":\"$LSP_ID\",\"addr\":\"$LSP_HOST\",\"port\":$LSP_PORT}")" | tee "$SIMDIR/lampo-connect-lsp.json"

echo "=== fund lampo if the wallet is empty (needs testnet coins in bitcoind) ==="
LAMPO_ADDR=$(lampo_call newaddr '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin).get("address",""))')
echo "lampo address: $LAMPO_ADDR"
# Operator must send testnet3 coins here if the wallet is empty. The script
# records the address and continues only when funds are visible.
for i in $(seq 1 60); do
  funds=$(lampo_call funds '{}' || echo '{}')
  if python3 -c 'import json,sys; d=json.loads(sys.argv[1]); sys.exit(0 if d.get("transactions") else 1)' "$funds"; then
    break
  fi
  echo "waiting for lampo funds at $LAMPO_ADDR ($i)"
  sleep 30
done

echo "=== lampo pays phoenix offer and reveals a contact ==="
lampo_call pay "$(python3 - << PY
import json
print(json.dumps({
  "invoice_str": "$PHOENIX_OFFER",
  "amount": ${AMOUNT_SAT} * 1000,
  "bolt12": {
    "payer_note": "lampo->phoenix contact reveal",
    "reveal_contact": True,
    "contact_label": "phoenix",
    "intro_node": "$LSP_ID",
  },
  "timeout": "large",
}))
PY
)" | tee "$SIMDIR/lampo-to-phoenix.json"

echo "=== lampo contacts ==="
lampo_call listcontacts '{}' | tee "$SIMDIR/lampo-contacts.json"

echo "=== phoenix incoming, then add contact from paymentHash ==="
curl -fsS -u ":${PHX_PASS}" "http://127.0.0.1:${PHX_PORT}/payments/incoming" \
  | tee "$SIMDIR/phoenix-incoming.json"
PAYHASH=$(python3 -c 'import json; rows=json.load(open("'"$SIMDIR/phoenix-incoming.json"'")); print(rows[0]["paymentHash"] if rows else "")')
echo "phoenix paymentHash: $PAYHASH"
phx_call contacts --data-urlencode "name=lampo" --data-urlencode "paymentHash=$PAYHASH" \
  | tee "$SIMDIR/phoenix-contact.json"

echo "=== phoenix pays the revealed payer offer back ==="
CONTACT_ID=$(python3 -c 'import json; print(json.load(open("'"$SIMDIR/phoenix-contact.json"'"))["contactId"])')
phx_call paycontact \
  --data-urlencode "contactId=$CONTACT_ID" \
  --data-urlencode "amountSat=$AMOUNT_SAT" \
  --data-urlencode "message=phoenix->lampo payback" \
  | tee "$SIMDIR/phoenix-payback.json"

echo "=== lampo contacts after payback ==="
lampo_call listcontacts '{}' | tee "$SIMDIR/lampo-contacts-after.json"
echo "PHOENIX CONTACTS INTEROP COMPLETE"
