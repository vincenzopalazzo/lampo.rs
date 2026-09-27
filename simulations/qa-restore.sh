#!/usr/bin/env bash
# qa-restore.sh — fund-loss restore cases for fs and VSS.
#
# Not a soak. Each case asks one question: after this fault, does the node
# come back with its channel, or refuse to start, instead of becoming a
# new node under the same identity?
#
# Requires a running regtest bitcoind (CORE_URL) and, for BACKEND=vss, a
# VSS server at VSS_URL. Never deletes lampod.pid.
#
#   BACKEND=fs  ./simulations/qa-restore.sh
#   BACKEND=vss VSS_URL=http://127.0.0.1:18080/vss ./simulations/qa-restore.sh
set -uo pipefail

REPO=${REPO:-$(cd "$(dirname "$0")/.." && pwd)}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$REPO/qa-restore-run}
BACKEND=${BACKEND:-fs}
VSS_URL=${VSS_URL:-}
API_BASE=${API_BASE:-18410}
P2P_BASE=${P2P_BASE:-30310}
CORE_URL=${CORE_URL:-http://127.0.0.1:18332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
LOG=$SIMDIR/qa.log
CSV=$SIMDIR/results.csv
mkdir -p "$SIMDIR"
: > "$CSV"

say() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }
rc() { echo "$(date -Iseconds),$BACKEND,$1,$2,$3" | tee -a "$CSV"; }
bcli() {
  curl -sS --max-time 30 --user "$CORE_USER:$CORE_PASS" \
    --data-binary "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" \
    "$CORE_URL/wallet/default"
}
mine() {
  local addr
  addr=$(bcli getnewaddress | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"])')
  bcli generatetoaddress "[6,\"$addr\"]" >/dev/null
}
rpc() { curl -sS --max-time 90 -X POST "http://127.0.0.1:$1/$2" -H 'content-type: application/json' -d "${3:-{}}"; }

write_conf() { # $1 dir $2 api $3 p2p
  mkdir -p "$1/regtest"
  cat > "$1/regtest/lampo.conf" <<EOF
network=regtest
port=$3
announce-addr=127.0.0.1
api-host=http://127.0.0.1
api-port=$2
backend=core
core-url=$CORE_URL
core-user=$CORE_USER
core-pass=$CORE_PASS
storage=$BACKEND
EOF
  if [ "$BACKEND" = vss ]; then
    printf 'storage-url=%s\n' "$VSS_URL" >> "$1/regtest/lampo.conf"
  fi
}

launch() { # $1 dir
  if command -v setsid >/dev/null 2>&1; then
    setsid nohup "$BIN" --data-dir "$1" --network regtest > "$1/node.log" 2>&1 < /dev/null &
  else
    nohup "$BIN" --data-dir "$1" --network regtest > "$1/node.log" 2>&1 < /dev/null &
  fi
  disown 2>/dev/null || true
}

wait_api() { # $1 port
  local i id
  for i in $(seq 1 45); do
    id=$(rpc "$1" getinfo | python3 -c 'import json,sys
try:
 d=json.load(sys.stdin); print(d.get("node_id",""))
except Exception:
 print("")')
    [ -n "$id" ] && { echo "$id"; return 0; }
    sleep 2
  done
  return 1
}

stop_dir() { # $1 dir
  local p
  p=$(pgrep -f "lampod-cli --data-dir $1 " | head -1 || true)
  [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
  sleep 1
}

channels() { # $1 port
  rpc "$1" channels | python3 -c 'import json,sys
d=json.load(sys.stdin)
print(len(d.get("channels",[])))'
}

say "QA restore backend=$BACKEND bin=$BIN"
[ -x "$BIN" ] || { say "missing binary $BIN"; exit 1; }
[ "$BACKEND" = vss ] && [ -z "$VSS_URL" ] && { say "BACKEND=vss needs VSS_URL"; exit 1; }
bcli getblockchaininfo | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["result"]["chain"]=="regtest"' \
  || { say "bitcoind at $CORE_URL is not regtest"; exit 1; }

# Mature a spendable balance. A fresh regtest wallet only has immature coinbase.
ADDR=$(bcli getnewaddress | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"])')
bcli generatetoaddress "[101,\"$ADDR\"]" >/dev/null

A=$SIMDIR/a
B=$SIMDIR/b
rm -rf "$A" "$B"
write_conf "$A" "$API_BASE" "$P2P_BASE"
write_conf "$B" $((API_BASE+1)) $((P2P_BASE+1))
# First launch writes the mnemonic and exits. Second launch is the node.
launch "$A"; sleep 2; launch "$A"
launch "$B"; sleep 2; launch "$B"
IDA=$(wait_api "$API_BASE") || { say "node A never came up"; tail -20 "$A/node.log"; exit 1; }
IDB=$(wait_api $((API_BASE+1))) || { say "node B never came up"; tail -20 "$B/node.log"; exit 1; }
say "A=$IDA"
say "B=$IDB"

# Fund A from the miner wallet.
AA=$(rpc "$API_BASE" new_addr | python3 -c 'import json,sys; print(json.load(sys.stdin)["address"])')
bcli sendtoaddress "[\"$AA\", 0.2]" >/dev/null
mine
sleep 8

export IDB PEER_PORT=$((P2P_BASE+1))
python3 -c 'import json,os; json.dump({"node_id":os.environ["IDB"],"addr":"127.0.0.1","port":int(os.environ["PEER_PORT"])}, open("/tmp/qa-connect.json","w")); json.dump({"node_id":os.environ["IDB"],"amount":100000,"public":True,"addr":"127.0.0.1","port":int(os.environ["PEER_PORT"])}, open("/tmp/qa-fund.json","w"))'
curl -sS --max-time 90 -X POST "http://127.0.0.1:$API_BASE/connect" -H 'content-type: application/json' --data-binary @/tmp/qa-connect.json >/dev/null
OPEN=$(curl -sS --max-time 90 -X POST "http://127.0.0.1:$API_BASE/fundchannel" -H 'content-type: application/json' --data-binary @/tmp/qa-fund.json)
echo "$OPEN" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert "tx" in d, d' || { say "fundchannel failed: $OPEN"; exit 1; }
mine
sleep 8
BEFORE=$(channels "$API_BASE")
say "channels before fault: $BEFORE"
[ "$BEFORE" -ge 1 ] || { say "no channel to restore"; exit 1; }

# Case 1: kill -9 and restart. Channel must still be listed.
say "R1 kill -9 then restart"
stop_dir "$A"
launch "$A"
wait_api "$API_BASE" >/dev/null || { rc R1 FAIL "node did not return"; exit 1; }
AFTER=$(channels "$API_BASE")
if [ "$AFTER" -ge 1 ]; then rc R1 PASS "channel survived kill -9 ($AFTER)"; else
  rc R1 FAIL "channel missing after kill -9"; say "FAIL R1"; exit 1
fi

# Case 2: wipe the remote/local channel manager and restart.
# Filesystem: delete manager. VSS: the local marker says this store was
# initialized, so a missing manager must refuse startup instead of minting
# a new node under the same identity.
say "R2 wiped manager must not look like a new funded node"
stop_dir "$A"
if [ "$BACKEND" = fs ]; then
  rm -f "$A/manager"
else
  # Drop every VSS object for this node's store id by pointing the URL at a
  # fresh empty server path is not possible here. Delete the remote keys by
  # removing the local marker's sibling and the VSS rows is operator-side.
  # The fail-closed check keys off the local marker plus a missing manager.
  # Simulate the wipe the code can see: keep the marker, and if we cannot
  # delete remote keys, skip the remote wipe and say so.
  if [ -f "$A/.lampo-vss-initialized" ]; then
    say "  VSS marker present; remote wipe is not performed by this script"
    say "  (refusing a wiped store is covered when the server returns NotFound)"
    rc R2 SKIP "remote wipe needs a server-side delete; marker is present"
  else
    rc R2 FAIL "VSS node has no initialization marker"
    exit 1
  fi
fi
if [ "$BACKEND" = fs ]; then
  launch "$A"
  sleep 6
  if NEW=$(wait_api "$API_BASE"); then
    # A wiped filesystem store has no local marker. LDK may start a new
    # node. That is only safe if the identity changed, so the old channels
    # are not claimed by an empty manager.
    NOW=$(channels "$API_BASE")
    if [ "$NEW" = "$IDA" ] && [ "$NOW" = 0 ]; then
      rc R2 FAIL "same node id, zero channels, after manager wipe (monitors left behind)"
      say "R2 is a fund-loss case: the node started instead of refusing"
      exit 1
    fi
    if [ "$NOW" -ge 1 ]; then
      rc R2 PASS "channel still present after manager wipe ($NOW)"
    else
      rc R2 PASS "wiped manager came back as a different node"
    fi
  else
    rc R2 PASS "wiped manager did not serve API"
  fi
fi

say "QA restore done"
cat "$CSV"
