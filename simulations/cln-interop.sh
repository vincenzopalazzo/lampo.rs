#!/opt/homebrew/bin/bash
#
# simulations/cln-interop.sh — lampo ↔ CLN ↔ LDK-Server three-implementation
# interop on regtest.
#
# Topology (dead-end for lp1<->lp2):
#   lp1 (lampo) ─c1─ c1n (CLN) ─c2─ lk1 (ldk) ─c3─ lp2 (lampo)
#
#   c1: lampo opens to CLN    (public, push -> CLN outbound)
#   c2: CLN  opens to ldk     (public, push -> ldk outbound)
#   c3: lampo opens to ldk    (public, push -> ldk outbound)
#
# Every lp1<->lp2 payment crosses all three implementations. Also covers
# BOLT12 both ways (CLN offer paid by lampo, lampo offer paid by CLN) and
# a CLN restart with lampo/ldk auto-reconnect.
#
# Prereqs: bitcoind regtest on CORE_URL (default wallet loaded); builds:
#   REPO/target/release/{lampod-cli,lampo-cli}
#   LDK_REPO/target/release/{ldk-server,ldk-server-cli}
#   CLN_BIN (lightningd)
#
# Env: REPO BIN SIMDIR API_BASE P2P_BASE CORE_URL CORE_USER CORE_PASS
#      CLN_BIN CLN_DIR CLN_P2P LDK_REPO LDKDIR LDK_GRPC_BASE LDK_P2P_BASE
#      CHANNEL_SAT PUSH_MSAT KEEP_GOING
set -uo pipefail

REPO=${REPO:-$PWD}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$REPO/sim-run-cln}
API_BASE=${API_BASE:-8310}
P2P_BASE=${P2P_BASE:-20100}
CORE_URL=${CORE_URL:-http://127.0.0.1:18332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
CLN_BIN=${CLN_BIN:-/usr/local/bin/lightningd}
CLN_CLI=${CLN_CLI:-/usr/local/bin/lightning-cli}
CLN_DIR=${CLN_DIR:-/tmp/lampo-cln1}
CLN_P2P=${CLN_P2P:-9736}
LDK_REPO=${LDK_REPO:-/Users/vincenzopalazzo/github/work/btc/ldk-server}
LDKDIR=${LDKDIR:-$LDK_REPO/ldk-nodes}
LDK_GRPC_BASE=${LDK_GRPC_BASE:-3540}
LDK_P2P_BASE=${LDK_P2P_BASE:-9840}
CHANNEL_SAT=${CHANNEL_SAT:-1000000}
PUSH_MSAT=${PUSH_MSAT:-400000000}
KEEP_GOING=${KEEP_GOING:-0}
TMO=${TMO:-60}

CSV=$SIMDIR/results.csv
LOG=$SIMDIR/cln-interop.log
mkdir -p "$SIMDIR"
: > "$LOG"

say() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }
ok()  { say "OK  $*"; echo "$(date -Iseconds),$1,OK,${2:-}" >> "$CSV"; }
ko()  { say "FAIL $* ${3:-}"; echo "$(date -Iseconds),$1,FAIL,${3:-}" >> "$CSV"
        tail -n 5 "$SIMDIR"/lp*/console.log >> "$LOG" 2>/dev/null
        [ "$KEEP_GOING" = 1 ] || exit 2; }
check() { local id=$1 desc=$2 cmd=$3; if eval "$cmd"; then ok "$id $desc"; else ko "$id $desc" "$cmd"; fi; }

bcli() { curl -sS --max-time 30 --user "$CORE_USER:$CORE_PASS" \
    --data-binary "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" \
    "$CORE_URL/wallet/default"; }
bcres() { bcli "$@" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(json.dumps(d.get("result") if "result" in d else d.get("error")))' 2>/dev/null; }
mine() { local a; a=$(bcres getnewaddress | tr -d '"'); bcli generatetoaddress "[${1:-6},\"$a\"]" >/dev/null 2>&1; }

rpc() { curl -sS --max-time "$TMO" -X POST "http://127.0.0.1:$1/$2" -H 'content-type: application/json' -d "${3:-{\}}"; }
jqf() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }

lcli() { # lcli <lkN> <subcmd...>
  local n=$1; shift
  local key port
  key=$(od -An -tx1 -v "$LDKDIR/$n/data/regtest/api_key" 2>/dev/null | tr -d ' \n')
  port=$(( LDK_GRPC_BASE + ${n#lk} ))
  "$LDK_REPO/target/release/ldk-server-cli" --base-url "127.0.0.1:$port" \
    --api-key "$key" --tls-cert "$LDKDIR/$n/data/tls.crt" "$@" 2>>"$LOG"
}
cln() { "$CLN_CLI" --lightning-dir="$CLN_DIR" "$@" 2>>"$LOG"; }

API() { case $1 in lp1) echo $API_BASE;; lp2) echo $((API_BASE + 2));; esac; }
P2PPORT() { case $1 in lp1) echo $((P2P_BASE + 1));; lp2) echo $((P2P_BASE + 2));; lk1) echo $((LDK_P2P_BASE + 1));; esac; }

say "cln-interop start: repo=$REPO simdir=$SIMDIR"

# ---- 0. chain up -----------------------------------------------------
[ -n "$(bcres getblockcount)" ] || { say "bitcoind not reachable on $CORE_URL"; exit 2; }
TIP=$(bcres getblockcount); say "bitcoind tip=$TIP"

# ---- 1. clean process state -----------------------------------------
pkill -x lampod-cli 2>/dev/null; pkill -x lightningd 2>/dev/null
for n in lk1 lk2; do pkill -f "ldk-server .*$LDKDIR/$n/config.toml" 2>/dev/null; done
sleep 2

# ---- 2. lampo lp1/lp2 ------------------------------------------------
for n in 1 2; do
  rm -rf "$SIMDIR/lp$n"; mkdir -p "$SIMDIR/lp$n/regtest"
  cat > "$SIMDIR/lp$n/regtest/lampo.conf" <<EOF
network=regtest
port=$(( P2P_BASE + n ))
announce-addr=127.0.0.1
api-host=http://127.0.0.1
api-port=$(( API_BASE + (n-1)*2 ))
backend=core
core-url=$CORE_URL
core-user=$CORE_USER
core-pass=$CORE_PASS
EOF
done
for round in 1 2; do
  for n in 1 2; do
    pgrep -f "lampod-cli --data-dir $SIMDIR/lp$n " >/dev/null || \
      nohup "$BIN" --data-dir "$SIMDIR/lp$n" --network regtest \
        >> "$SIMDIR/lp$n/console.log" 2>&1 &
    disown 2>/dev/null || true
  done
  sleep 6
done
LP1=$(rpc "$(API lp1)" getinfo | jqf 'd["node_id"]')
LP2=$(rpc "$(API lp2)" getinfo | jqf 'd["node_id"]')
[ -n "$LP1" ] && [ -n "$LP2" ] || { say "lampo nodes not up"; exit 2; }
say "lp1=$LP1"
say "lp2=$LP2"

# ---- 3. CLN ----------------------------------------------------------
rm -rf "$CLN_DIR"; mkdir -p "$CLN_DIR"
cat > "$CLN_DIR/config" <<EOF
network=regtest
bitcoin-rpcuser=$CORE_USER
bitcoin-rpcpassword=$CORE_PASS
bitcoin-rpcport=$(echo "$CORE_URL" | awk -F: '{print $3}')
bitcoin-rpcconnect=127.0.0.1
addr=127.0.0.1:$CLN_P2P
log-level=debug
rpc-file=$CLN_DIR/lightning-rpc
EOF
nohup "$CLN_BIN" --lightning-dir="$CLN_DIR" >> "$CLN_DIR/console.log" 2>&1 &
disown 2>/dev/null || true
sleep 8
C1N=$(cln getinfo | jqf 'd["id"]')
[ -n "$C1N" ] || { say "CLN not up"; exit 2; }
say "c1n=$C1N"

# ---- 4. LDK lk1 (fresh dir so a stale node_id never survives) --------
pkill -f "ldk-server .*$LDKDIR/lk1/config.toml" 2>/dev/null; sleep 1
rm -rf "$LDKDIR/lk1"; mkdir -p "$LDKDIR/lk1"
cat > "$LDKDIR/lk1/config.toml" <<EOF
[node]
network = "regtest"
listening_addresses = ["127.0.0.1:$(( LDK_P2P_BASE + 1 ))"]
announcement_addresses = ["127.0.0.1:$(( LDK_P2P_BASE + 1 ))"]
grpc_service_address = "127.0.0.1:$(( LDK_GRPC_BASE + 1 ))"
alias = "lk1"
pathfinding_scores_source_url = ""

[storage.disk]
dir_path = "$LDKDIR/lk1/data"

[log]
level = "Debug"

[bitcoind]
rpc_address = "127.0.0.1:$(echo "$CORE_URL" | awk -F: '{print $3}')"
rpc_user = "$CORE_USER"
rpc_password = "$CORE_PASS"
EOF
nohup "$LDK_REPO/target/release/ldk-server" "$LDKDIR/lk1/config.toml" \
  > "$LDKDIR/lk1/console.log" 2>&1 &
disown 2>/dev/null || true
LK1=""
for _ in $(seq 1 20); do
  LK1=$(lcli lk1 get-node-info | jqf 'd["node_id"]')
  [ -n "$LK1" ] && break
  sleep 4
done
[ -n "$LK1" ] || { say "lk1 not up"; exit 2; }
say "lk1=$LK1"

# ---- 5. fund everyone ------------------------------------------------
fund() { # fund <addr> <btc>
  bcres sendtoaddress "[\"$1\", $2]" >/dev/null 2>&1 || return 1
  mine 6; return 0
}
for n in 1 2; do
  A=$(rpc "$(API lp$n)" new_addr | jqf 'd["address"]')
  fund "$A" 5 || ko F-fund-lp$n "new_addr/fund"
done
CA=$(cln newaddr | jqf 'd["bech32"]')
fund "$CA" 5 || ko F-fund-cln "newaddr/fund"

# CLN indexes its own wallet; fundchannel fails with "0 available UTXOs"
# until the confirmed onchain balance is credited. Wait for it.
cln_wait_balance() {
  local i b
  for i in $(seq 1 24); do
    b=$(cln listfunds | jqf 'sum(1 for o in d.get("outputs",[]) if o.get("status")=="confirmed")')
    [ "${b:-0}" -ge 1 ] 2>/dev/null && return 0
    sleep 5
  done
  return 1
}
cln_wait_balance || ko F-cln-balance "CLN onchain balance never confirmed"
say "CLN onchain balance ready"
LA=$(lcli lk1 onchain-receive | python3 -c 'import json,sys
def find(o):
    if isinstance(o,dict):
        for k,v in o.items():
            if k=="address" and isinstance(v,str) and v.startswith("bcrt"): return v
        for v in o.values():
            r=find(v)
            if r: return r
    if isinstance(o,list):
        for v in o:
            r=find(v)
            if r: return r
print(find(json.load(sys.stdin)) or "")')
fund "$LA" 5 || ko F-fund-lk1 "onchain-receive/fund"
ok F-fund "all wallets funded"

# ---- 6. connect + channels ------------------------------------------
check N-connect-lp1-cln "lp1 connects CLN" \
  "rpc \"\$(API lp1)\" connect \"{\\\"node_id\\\":\\\"$C1N\\\",\\\"addr\\\":\\\"127.0.0.1\\\",\\\"port\\\":$CLN_P2P}\" >/dev/null"
check N-connect-cln-lk1 "CLN connects lk1" \
  "cln connect \"\$LK1@127.0.0.1:\$((LDK_P2P_BASE+1))\" >/dev/null"
check N-connect-lp2-lk1 "lp2 connects lk1" \
  "rpc \"\$(API lp2)\" connect \"{\\\"node_id\\\":\\\"$LK1\\\",\\\"addr\\\":\\\"127.0.0.1\\\",\\\"port\\\":\$((LDK_P2P_BASE+1))}\" >/dev/null"
sleep 3

check C-open-c1 "c1 lp1->CLN open" \
  "rpc \"\$(API lp1)\" fundchannel \"{\\\"node_id\\\":\\\"$C1N\\\",\\\"amount\\\":\$CHANNEL_SAT,\\\"public\\\":true,\\\"push_msat\\\":\$PUSH_MSAT}\" >/dev/null"
check C-open-c2 "c2 CLN->lk1 open" \
  "cln fundchannel id=\$LK1 amount=\$CHANNEL_SAT announce=true push_msat=\$PUSH_MSAT"
check C-open-c3 "c3 lp2->lk1 open" \
  "rpc \"\$(API lp2)\" fundchannel \"{\\\"node_id\\\":\\\"$LK1\\\",\\\"amount\\\":\$CHANNEL_SAT,\\\"public\\\":true,\\\"push_msat\\\":\$PUSH_MSAT}\" >/dev/null"
mine 6

lampo_pay_ok() {
  local res; res=$(TMO=120 rpc "$(API "$1")" pay "$2"); echo "$res" >> "$LOG"
  [ "$(echo "$res" | jqf 'd.get("state","")')" = Success ] && \
    [ -n "$(echo "$res" | jqf 'd.get("payment_preimage") or ""')" ]
}
lampo_keysend_ok() { # <src> <dst-id> <amt_msat>
  local res; res=$(TMO=120 rpc "$(API "$1")" keysend "{\"destination\":\"$2\",\"amount_msat\":$3}"); echo "$res" >> "$LOG"
  [ "$(echo "$res" | jqf 'd.get("state","")')" = Success ] && \
    [ -n "$(echo "$res" | jqf 'd.get("payment_preimage") or ""')" ]
}
ready() { # ready <side> -> 0 when every cN reports ready
  local i
  for i in 1 2 3; do
    case $i in
      1) [ "$(rpc "$(API lp1)" channels | jqf 'sum(1 for c in d["channels"] if c["ready"])')" -ge 1 ] || return 1;;
      2) [ "$(cln listchannels 2>/dev/null | jqf 'sum(1 for c in d.get("channels",[]) if c.get("short_channel_id") and c.get("state","").lower() in ("chanlenormal","normal","active") or c.get("active"))')" -ge 1 ] || return 1;;
      3) [ "$(rpc "$(API lp2)" channels | jqf 'sum(1 for c in d["channels"] if c["ready"])')" -ge 1 ] || return 1;;
    esac
  done
  return 0
}
r=0; for i in $(seq 1 30); do ready && break; sleep 5; r=1; done
if ready; then ok C-ready "c1..c3 ready"; else ko C-ready "channels not ready in 150s"; fi

# Gossip settle: public channels must be announced and seen by every node
# before multi-hop routing works (ldk-node + CLN + lampo all gossip). Probe
# with a tiny real keysend until it succeeds (bounded), then run the matrix.
probe_route() {
  local i
  for i in $(seq 1 18); do
    if lampo_keysend_ok lp1 "$LP2" 3000; then return 0; fi
    sleep 15
  done
  return 1
}
r=0; for i in $(seq 1 2); do probe_route && break; r=1; done
if [ "$r" = 0 ]; then ok G-gossip-settled "route lp1->lp2 live (probe paid)"; else ko G-gossip-settled "no route after ~9min"; fi

# ---- 7. payments across three implementations ------------------------
inv_lp2() { rpc "$(API lp2)" invoice "{\"amount_msat\":$1,\"description\":\"cln-interop\"}" | jqf 'd.get("bolt11","")'; }
inv_lp1() { rpc "$(API lp1)" invoice "{\"amount_msat\":$1,\"description\":\"cln-interop\"}" | jqf 'd.get("bolt11","")'; }
cln_pay_b12() {
  local inv; inv=$(cln fetchinvoice "$1" | jqf 'd["invoice"]')
  [ -n "$inv" ] || return 1
  cln pay "$inv" | jqf 'd.get("status","")' | grep -qi complete
}

P01_INV=$(inv_lp2 100000)
check P01-bolt11-lp1-to-lp2 "lp1->lp2 bolt11 3-impl" \
  "lampo_pay_ok lp1 \"{\\\"invoice_str\\\":\\\"\$P01_INV\\\"}\""

P02_INV=$(inv_lp1 120000)
check P02-bolt11-lp2-to-lp1 "lp2->lp1 bolt11 3-impl" \
  "lampo_pay_ok lp2 \"{\\\"invoice_str\\\":\\\"\$P02_INV\\\"}\""

check P03-keysend-lp1-to-lp2 "lp1->lp2 keysend 3-impl" \
  "lampo_keysend_ok lp1 \"\$LP2\" 90000"
check P04-keysend-lp2-to-lp1 "lp2->lp1 keysend 3-impl" \
  "lampo_keysend_ok lp2 \"\$LP1\" 80000"

# BOLT12: CLN offer paid by lampo (direct c1)
B01_OFFER=$(cln offer 150000 "cln-offer-interop" | jqf 'd["bolt12"]')
check B01-bolt12-cln-offer-lampo-pays "lp1 pays CLN offer" \
  "lampo_pay_ok lp1 \"{\\\"invoice_str\\\":\\\"\$B01_OFFER\\\"}\""

# BOLT12: lampo offer paid by CLN (2 hops lp2 <- lk1 <- CLN)
B02_OFFER=$(rpc "$(API lp2)" offer "{\"amount_msat\":130000,\"description\":\"lampo-offer-interop\"}" | jqf 'd.get("bolt12","")')
check B02-bolt12-lampo-offer-cln-pays "CLN pays lp2 offer" \
  "cln fetchinvoice \"\$B02_OFFER\" >/dev/null && cln_pay_b12 \"\$B02_OFFER\""

# ---- 8. CLN restart; lampo reconnect ---------------------------------
check R01-cln-restart "CLN restart" \
  "pkill -x lightningd; sleep 3; nohup \"\$CLN_BIN\" --lightning-dir=\"\$CLN_DIR\" >> \"\$CLN_DIR/console.log\" 2>&1 & disown; sleep 8; [ -n \"\$(cln getinfo | jqf 'd[\"id\"]')\" ]"
r=0; for i in $(seq 1 24); do ready && break; sleep 5; r=1; done
if ready; then ok R02-ready-after-restart "channels ready after CLN restart"; else ko R02-ready-after-restart "channels not ready 120s after restart"; fi

P03_INV=$(inv_lp2 110000)
check P05-bolt11-after-restart "lp1->lp2 after restart" \
  "lampo_pay_ok lp1 \"{\\\"invoice_str\\\":\\\"\$P03_INV\\\"}\""

# ---- 9. log health ---------------------------------------------------
if grep -qiE "panic|corrupt state" "$SIMDIR"/lp1/console.log "$SIMDIR"/lp2/console.log 2>/dev/null; then
  ko L01-log-health "panic/corruption in lampo logs"
else
  ok L01-log-health "no panics in lampo logs"
fi

say "CLN-INTEROP COMPLETE: $(grep -c ',OK,' "$CSV") OK / $(grep -c ',FAIL,' "$CSV") FAIL"
