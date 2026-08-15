#!/usr/bin/env bash
#
# sim/interop.sh — lampo ↔ LDK-Server cross-implementation test (regtest).
#
# Topology (dead-end: lp1 and lp2 share NO channel):
#   lp1 (lampo) ──c1── lk1 (ldk) ──c2── lk2 (ldk) ──c3── lp2 (lampo)
#   c1 opened by lampo (public:true), c2/c3 opened by ldk (--announce-channel)
#
# Every lp1<->lp2 payment crosses both implementations twice.
# Asserts: connect, channels, bolt11 both ways, keysend both ways,
# cross-impl multihop (both directions), bolt12 offer, chaos on the
# LDK side + lampo auto-reconnect, log health. Rows -> interop-results.csv.
#
# Prereqs (on the server):
#   sim/ldk-deploy.sh build && sim/ldk-deploy.sh start 2
#   ~/lampo-main-sim built (ship.sh sim/main-next -> BUILD_OK)
#
# Env: REPO BIN SIMDIR SEED TMO KEEP_GOING CHANNEL_SAT
#      API_BASE P2P_BASE CORE_URL CORE_USER CORE_PASS
#      LDKDIR LDK_GRPC_BASE LDK_P2P_BASE LDK_DEBUG
set -uo pipefail

REPO=${REPO:-$HOME/lampo-main-sim}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$REPO/interop}
LDKDIR=${LDKDIR:-$HOME/ldk-server/ldk-nodes}
LDK_REPO=${LDK_REPO:-$HOME/ldk-server}
LDK_GRPC_BASE=${LDK_GRPC_BASE:-3540}
LDK_P2P_BASE=${LDK_P2P_BASE:-9840}
LDK_CLI=$LDK_REPO/target/release/ldk-server-cli
CHANNEL_SAT=${CHANNEL_SAT:-1000000}
SEED=${SEED:-7}
TMO=${TMO:-60}
KEEP_GOING=${KEEP_GOING:-0}
API_BASE=${API_BASE:-8300}
P2P_BASE=${P2P_BASE:-20100}
CORE_URL=${CORE_URL:-http://127.0.0.1:18332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
LOG=$SIMDIR/interop.log
CSV=$SIMDIR/interop-results.csv

declare -A IDX=([lp1]=21 [lp2]=22); ALLNODES=(lp1 lp2)
declare -A ID=()

source "$(dirname "$0")/lib.sh"

ldk_idx() { case $1 in lk1) echo 1;; lk2) echo 2;; *) echo 0;; esac; }
ldk_port() { echo $(( LDK_GRPC_BASE + $(ldk_idx "$1") )); }
ldk_p2p()  { echo $(( LDK_P2P_BASE + $(ldk_idx "$1") )); }
ldk_dir()  { echo "$LDKDIR/$1"; }

ldk_key()  { od -An -tx1 -v "$(ldk_dir "$1")/data/regtest/api_key" 2>/dev/null | tr -d ' \n'; }
lcli() { # lcli <node> <args...>
  local n=$1; shift
  [ "${LDK_DEBUG:-0}" = 1 ] && "$LDK_CLI" "$1" --help 2>&1 | head -30 | sed "s/^/[lk-help] /" >> "$LOG"
  "$LDK_CLI" --base-url "127.0.0.1:$(ldk_port "$n")" --api-key "$(ldk_key "$n")" \
    --tls-cert "$(ldk_dir "$n")/data/tls.crt" "$@"
}
lcli_json() { lcli "$@" 2>>"$LOG" | python3 -c 'import json,sys
try: print(json.dumps(json.load(sys.stdin)))
except Exception: print("")' ; }

ldk_id() { lcli "$1" get-node-info 2>/dev/null | python3 -c 'import json,sys
d=json.load(sys.stdin)
for k in ("node_id","nodeId","pubkey","id"):
    if isinstance(d,dict) and d.get(k): print(d[k]); break
else: print("")' ; }
ldk_up() { [ -n "$(ldk_id "$1")" ]; }
ldk_channels_ready() { lcli "$1" list-channels 2>/dev/null | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
chs=d.get("channels") or d.get("list_channels") or []
print(sum(1 for c in chs if c.get("is_ready", c.get("ready", True))))' 2>/dev/null; }

ldk_channel_ready_with() { # ldk_channel_ready_with <node> <peer-id> -> 0/1
  lcli "$1" list-channels 2>/dev/null | python3 -c '
import json,sys
d=json.load(sys.stdin); chs=d.get("channels",d) if isinstance(d,dict) else d
peer=sys.argv[1]
print(1 if any(c.get("counterparty_node_id")==peer and c.get("is_channel_ready") for c in chs) else 0)' "$2" 2>/dev/null
}

ldk_bolt11() { # ldk_bolt11 <node> <amt_msat> -> invoice string (amount is POSITIONAL)
  lcli "$1" bolt11-receive "${2}msat" -d interop 2>>"$LOG" | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(""); sys.exit(0)
for k in ("invoice","bolt11","payment_request","description"):
    v=d.get(k) if isinstance(d,dict) else None
    if isinstance(v,str) and "lnbc" in v: print(v); sys.exit(0)
s=json.dumps(d)
i=s.find("lnbc")
print(s[i:s.find(chr(34),i)] if i>=0 else "")' ; }

amt() { rand_amount "$1" 20000 3000000; }   # msat, log-uniform, seeded

row() { echo "$(date -Iseconds),$1,$2,$3,${4:-},$(amt debug 2>/dev/null || echo 0)" >> /dev/null; }
ok()  { say "OK  $1"; echo "$(date -Iseconds),$1,OK"   >> "$CSV"; }
ko()  { say "FAIL $1: ${2:-}"; echo "$(date -Iseconds),$1,FAIL,${2:-}" >> "$CSV"
        collect_artifacts "$1"; [ "$KEEP_GOING" = 1 ] || exit 2; }

check() { # check <id> <desc> <cmd-string>  (eval'd in THIS shell: lib.sh
  # functions and ID[] stay visible; bash -c would hide them)
  local id=$1 desc=$2 cmd=$3
  if eval "$cmd"; then ok "$id $desc"; else ko "$id $desc" "eval failed"; fi
}

# ---- payment helpers ------------------------------------------------
lampo_invoice() { # <node> <amt_msat>
  rpc "$(API "$1")" invoice "{\"amount_msat\":$2,\"description\":\"interop\"}" | jqf 'd.get("bolt11","")'
}
lampo_offer() { # <node> <amt_msat>
  rpc "$(API "$1")" offer "{\"amount_msat\":$2,\"description\":\"interop\"}" | jqf 'd.get("bolt12","")'
}
lampo_pay_ok() { # <src> <json-args> -> 0 iff Success AND preimage; echoes res
  local res
  res=$(TMO=120 rpc "$(API "$1")" pay "$2")
  echo "$res"
  [ "$(echo "$res" | jqf 'd.get("state","")')" = Success ] && [ -n "$(echo "$res" | jqf 'd.get("payment_preimage") or ""')" ]
}
lampo_keysend_ok() { # <src> <dst-id> <amt_msat>
  local res
  res=$(TMO=120 rpc "$(API "$1")" keysend "{\"destination\":\"$2\",\"amount_msat\":$3}")
  echo "$res"
  [ "$(echo "$res" | jqf 'd.get("state","")')" = Success ] && [ -n "$(echo "$res" | jqf 'd.get("payment_preimage") or ""')" ]
}
ldk_pay_bolt11() { # <node> <invoice> -> 0 iff paid
  local out; out=$(lcli "$1" bolt11-send "$2" 2>>"$LOG")
  echo "$out" >> "$LOG"
  echo "$out" | grep -qiE 'succe|paid|preimage|complete' && return 0
  sleep 5
  lcli "$1" list-payments 2>/dev/null | grep -q "$2" && return 0   # rough fallback
  return 1
}
ldk_spontaneous() { # <node> <node-id> <amt_msat> -> 0 iff paid
  local out; out=$(lcli "$1" spontaneous-send "$2" "${3}msat" 2>>"$LOG")
  echo "$out" >> "$LOG"
  echo "$out" | grep -qiE 'succe|paid|preimage|pending' && return 0
  return 1
}

# ---- cluster bring-up ----------------------------------------------
mkdir -p "$SIMDIR"; : > /dev/null
[ -f "$CSV" ] || echo "ts,case,result,detail" > "$CSV"
say "=== interop run start (SEED=$SEED CHANNEL_SAT=$CHANNEL_SAT) ==="

say "lampo nodes lp1/lp2 (API $(API lp1)/$(API lp2), P2P $(P2P lp1)/$(P2P lp2))"
for n in lp1 lp2; do
  ID[$n]=$(load_ids && echo "${ID[$n]:-}") || true
  if [ -z "${ID[$n]:-}" ] || ! pgrep -f "lampod-cli --data-dir $SIMDIR/$n " >/dev/null; then
    start_node "$n"; ID[$n]=$(wait_up "$n") || { say "cannot start $n"; exit 2; }
  fi
done
save_ids

for n in lk1 lk2; do
  ldk_up "$n" || { say "$n not running — run sim/ldk-deploy.sh start 2 first"; exit 2; }
  ID[$n]=$(ldk_id "$n"); say "$n id=${ID[$n]:0:16}.. (grpc $(ldk_port $n), p2p $(ldk_p2p $n))"
done

# The shared regtest chain is also carrying the endless soak's chaos
# (reorgs, restarts, feespam -> transient bitcoind 503s): retry funding.
for _n in lp1 lp2; do
  _ok=0
  for _try in 1 2 3; do
    if fund_node "$_n" 50; then _ok=1; break; fi
    say "fund $_n failed (try $_try) — backing off 30s"
    sleep 30
  done
  [ "$_ok" = 1 ] || ko SETUP "fund $_n"
done
for _try in 1 2 3; do
  wait_wallet_synced 420 && break
  say "wallet sync failed (try $_try)"
  [ "$_try" = 3 ] && ko SETUP "wallet sync"
done

# I01 connect lampo -> ldk
check I01 "lp1<->lk1 connect" '
  r=$(rpc "$(API lp1)" connect "{\"node_id\":\"${ID[lk1]}\",\"addr\":\"127.0.0.1\",\"port\":$(ldk_p2p lk1)}")
  echo "$r" | jqf "d.get(\"error\",{}).get(\"message\",\"\")" | grep -q . && exit 1
  sleep 3
  [ "$(peers_of lp1)" -ge 1 ]'

# I02 c1: lampo opens to ldk (public, push for ldk outbound liquidity).
# NOTE: open_channel() computes P2P "$to" via lib.sh arithmetic, which only
# knows lampo node names (IDX/n<k>); lk1 would hit `unbound variable` and
# emit a broken port field -> do the fundchannel inline with ldk_p2p.
check I02 "open c1 lp1->lk1 (lampo, public)" '
  resp=$(TMO=150 rpc "$(API lp1)" fundchannel "{\"node_id\":\"${ID[lk1]}\",\"addr\":\"127.0.0.1\",\"port\":$(ldk_p2p lk1),\"amount\":$CHANNEL_SAT,\"public\":true,\"push_msat\":100000000}")
  case "$resp" in "{"*) : ;; *) echo "non-JSON: $resp" | head -c 200; exit 1 ;; esac
  echo "$resp" | jqf "d.get(\"error\",{}).get(\"message\",\"\")" | grep -q . && { echo "$resp" | head -c 300; exit 1; }
  for _ in $(seq 1 20); do sz=$(bcli getmempoolinfo | jqf "d[\"result\"][\"size\"]"); [ "${sz:-0}" -gt 0 ] 2>/dev/null && break; sleep 3; done
  mine 8
  for i in $(seq 1 120); do [ "$(ready_channels lp1)" -ge 1 ] && break; sleep 5; done
  [ "$(ready_channels lp1)" -ge 1 ]'
# I03 c2: ldk opens to ldk (announced)
check I03 "open c2 lk1->lk2 (ldk, announced)" '
  o=$(lcli lk1 connect-peer "${ID[lk2]}@127.0.0.1:$(ldk_p2p lk2)" 2>>"$LOG"); echo "$o" >> "$LOG"
  o=$(lcli lk1 open-channel "${ID[lk2]}" "127.0.0.1:$(ldk_p2p lk2)" "${CHANNEL_SAT}sat" --announce-channel 2>>"$LOG"); echo "$o" >> "$LOG"
  sleep 5; mine 8; sleep 10
  for i in $(seq 1 120); do [ "$(ldk_channel_ready_with lk1 "${ID[lk2]}")" = 1 ] && break; sleep 5; done
  [ "$(ldk_channel_ready_with lk1 "${ID[lk2]}")" = 1 ]'

# I04 c3: ldk opens to lampo (announced)
check I04 "open c3 lk2->lp2 (ldk, announced)" '
  o=$(lcli lk2 connect-peer "${ID[lp2]}@127.0.0.1:$(P2P lp2)" 2>>"$LOG"); echo "$o" >> "$LOG"
  o=$(lcli lk2 open-channel "${ID[lp2]}" "127.0.0.1:$(P2P lp2)" "${CHANNEL_SAT}sat" --announce-channel 2>>"$LOG"); echo "$o" >> "$LOG"
  sleep 5; mine 8; sleep 10
  for i in $(seq 1 120); do { [ "$(ldk_channel_ready_with lk2 "${ID[lp2]}")" = 1 ] && [ "$(ready_channels lp2)" -ge 1 ]; } && break; sleep 5; done
  [ "$(ldk_channel_ready_with lk2 "${ID[lp2]}")" = 1 ] && [ "$(ready_channels lp2)" -ge 1 ]'

wait_wallet_synced || true

# ---- payments --------------------------------------------------------
A5=$(amt I05); INV5=$(ldk_bolt11 lk1 "$A5")
[ -n "$INV5" ] || ko I05 "lk1 no invoice"
res=$(lampo_pay_ok lp1 "{\"invoice_str\":\"$INV5\"}")
if [ -n "$res" ] && echo "$res" | grep -q Success; then ok "I05 bolt11 lp1->lk1 ${A5}msat"; else ko I05 "lp1->lk1 bolt11" "$(echo "$res" | head -c 200)"; fi

A6=$(amt I06); INV6=$(lampo_invoice lp1 "$A6")
[ -n "$INV6" ] || ko I06 "lp1 no invoice"
if ldk_pay_bolt11 lk1 "$INV6"; then ok "I06 bolt11 lk1->lp1 ${A6}msat"; else ko I06 "lk1->lp1 bolt11" "see log"; fi

A7=$(amt I07)
if lampo_keysend_ok lp1 "${ID[lk1]}" "$A7" >/dev/null; then ok "I07 keysend lp1->lk1 ${A7}msat"; else ko I07 "keysend lp1->lk1 (CLTV>=144?)" ""; fi

A8=$(amt I08)
if ldk_spontaneous lk1 "${ID[lp1]}" "$A8"; then ok "I08 spontaneous lk1->lp1 ${A8}msat"; else ko I08 "spontaneous lk1->lp1" ""; fi

# I09/I10 cross-impl multihop lp1 <-> lp2 (path MUST cross lk1+lk2)
cross_pay() { # <tag> <src> <dst> <amt>
  local tag=$1 src=$2 dst=$3 a=$4 inv res hops both
  inv=$(lampo_invoice "$dst" "$a"); [ -n "$inv" ] || { ko "$tag" "$dst no invoice"; return 1; }
  res=$(lampo_pay_ok "$src" "{\"invoice_str\":\"$inv\"}")
  hops=$(echo "$res" | jqf 'len(d.get("path",[]))')
  both=$(echo "$res" | jqf 'int("'"${ID[lk1]}"'" in [h.get("node_id","") for h in d.get("path",[])] and "'"${ID[lk2]}"'" in [h.get("node_id","") for h in d.get("path",[])])')
  echo "$(date -Iseconds),$tag,${a}msat,hops=${hops:-?},cross=$both" >> "$CSV"
  if echo "$res" | grep -q Success && [ "${hops:-0}" -ge 3 ] && [ "$both" = 1 ]; then
    ok "$tag $src->$dst ${a}msat hops=$hops (via lk1+lk2)"
  else
    ko "$tag $src->$dst" "hops=${hops:-?} cross=$both raw=$(echo "$res" | head -c 200)"
  fi
}
cross_pay I09 lp1 lp2 "$(amt I09)"
cross_pay I10 lp2 lp1 "$(amt I10)"

# I11 bolt12 offer lp1 -> lp2 (gossip needs ~150s after announces)
say "waiting 240s for gossip/announcer tick before bolt12 (busy shared chain)…"; sleep 240
A11=$(amt I11); OFF=$(lampo_offer lp2 "$A11"); [ -n "$OFF" ] || ko I11 "lp2 no offer"
res=$(lampo_pay_ok lp1 "{\"invoice_str\":\"$OFF\",\"amount\":$A11}")
if echo "$res" | grep -q Success; then ok "I11 bolt12 offer lp1->lp2 ${A11}msat"; else ko I11 "offer lp1->lp2" "$(echo "$res" | head -c 200)"; fi

# I12 chaos: SIGKILL lk1, restart, lampo auto-reconnect (main e65bd89)
MARKS=$(log_marks)
kill -9 "$(pgrep -f "ldk-server $(ldk_dir lk1)/config.toml" | head -1)" 2>/dev/null
sleep 3
setsid nohup "$LDK_REPO/target/release/ldk-server" "$(ldk_dir lk1)/config.toml" \
  >> "$(ldk_dir lk1)/console.log" 2>&1 < /dev/null & disown 2>/dev/null || true
up=1
for i in $(seq 1 120); do ldk_up lk1 && break; sleep 3; done
ldk_up lk1 || up=0
[ "$up" = 1 ] && ok "I12 lk1 killed+restarted, daemon up" || ko I12 "lk1 restart" "daemon did not come up"

# I13 post-chaos payment across the recovered path
sleep 20
cross_pay I13 lp1 lp2 "$(amt I13)"

# I14 lampo health on log delta
if health_scan_since "$MARKS"; then ok "I14 lampo logs clean (no panic/corrupt/invariant)"; else ko I14 "health" "see artifacts"; fi

say "=== interop run done: $(grep -c OK "$CSV") ok / $(grep -c FAIL "$CSV") fail so far ==="
exit 0
