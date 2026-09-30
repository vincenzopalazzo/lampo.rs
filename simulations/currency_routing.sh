#!/usr/bin/env bash
#
# simulations/currency_routing.sh — BOLT 12 currency + routing soak.
#
# Star topology: one central ROUTER (r) with channels to every edge node
# (e1..e5). No edge-edge channels exist, so every edge<->edge payment is a
# structural multi-hop (eX -> r -> eY), never direct. The router carries 100%
# of forwarded volume, and the chaos leg kills and restarts it mid-soak.
#
# Currency emphasis: most rounds pay BOLT 12 offers denominated in ISO 4217
# minor units (USD cents / EUR cents) through LampoCurrencyConversion (lampo
# PR #627, lightning fork branch lampo/bolt12-currency-0.3). Baselines
# (bolt11, msat-denominated offers) prove the router — not the currency path —
# is at fault on failure. Negative legs prove unknown codes and
# out-of-tolerance explicit amounts are rejected locally, before any onion
# message is sent.
#
# Regtest only (needs mining control). Ports API 8410+ / P2P 20310+, node data
# $SIMDIR/{r,e1..e5} (lib.sh node_dir) and results in $SIMDIR/cr: disjoint from simulate.sh (810x), multihop (821x) and
# mutinet (811x) soaks — all can run at once on the same host.
#
# Env: CR_ROUNDS(15) SEED(42) CHAOS_EVERY(5)
#      CURRENCY_RATES(USD=1000,EUR=1100) CURRENCY_TOLERANCE_BPS(100)
#      CR_CHANNEL_AMT_SATS(1000000) KEEP_GOING(0) TMO(90) (+ lib.sh env)
#
# Gates (mirrors Phase 2):
# - every pay asserts state=="Success" AND preimage (never log prose)
# - every edge sends AND receives (edge-role matrix + CSV coverage gate)
# - router bounce preserves node_id
# - final line CURRENCY-ROUTING COMPLETE after edge coverage OK
#
# Usage:
#   CR_ROUNDS=2 CHAOS_EVERY=99 ./simulations/currency_routing.sh  # smoke
#   NODES/ROUNDS style: CR_ROUNDS=20 CHAOS_EVERY=3 ./simulations/currency_routing.sh
set -uo pipefail

REPO=${REPO:-$HOME/lampo-sim}
BIN=${BIN:-$REPO/target/release/lampod-cli}
SIMDIR=${SIMDIR:-$REPO/sim-run}
CRDIR=${CRDIR:-$SIMDIR/cr}
LOG=${LOG:-$CRDIR/sim.log}
CSV=${CSV:-$CRDIR/results.csv}
CR_ROUNDS=${CR_ROUNDS:-15}
SEED=${SEED:-42}
CHAOS_EVERY=${CHAOS_EVERY:-5}
CURRENCY_RATES=${CURRENCY_RATES:-USD=1000,EUR=1100}
CURRENCY_TOLERANCE_BPS=${CURRENCY_TOLERANCE_BPS:-100}
CR_CHANNEL_AMT_SATS=${CR_CHANNEL_AMT_SATS:-1000000}
API_BASE=${API_BASE:-8410}
P2P_BASE=${P2P_BASE:-20310}
CORE_URL=${CORE_URL:-http://127.0.0.1:18332}
CORE_USER=${CORE_USER:-testutil}
CORE_PASS=${CORE_PASS:-testutilpassword}
TMO=${TMO:-90}
KEEP_GOING=${KEEP_GOING:-0}

source "$(dirname "$0")/lib.sh"

declare -A IDX=([r]=1 [e1]=2 [e2]=3 [e3]=4 [e4]=5 [e5]=6)
ROUTER=r
EDGES=(e1 e2 e3 e4 e5)
ALLNODES=(r e1 e2 e3 e4 e5)
declare -A ID=()

# Ratetable mirror for CSV expectation columns only. Enforcement lives in
# LDK (Amount::to_msats_range) via the node's own currency-rates config.
declare -A RATE=()
for entry in ${CURRENCY_RATES//,/ }; do
  RATE[${entry%%=*}]=${entry##*=}
done

exp_range() { # $1 CODE $2 minor -> "ref min max"
  python3 -c "rate=${RATE[$1]};tol=$CURRENCY_TOLERANCE_BPS;minor=$2;ref=rate*minor;d=ref*tol//10000;print(ref,ref-d,ref+d)"
}

cr_offer() { # $1 dst $2 code $3 minor $4 desc -> offer string (may be empty)
  rpc "$(API "$1")" offer "{\"description\":\"$4\",\"currency\":\"$2\",\"currency_amount\":$3}" \
    | jqf 'd.get("bolt12","")'
}

cr_pay_currency() { # $1 tag $2 src $3 dst $4 code $5 minor [$6 explicit_msat]
  local tag=$1 src=$2 dst=$3 code=$4 minor=$5 explicit=${6:-}
  local offer rng ref exp_min exp_max body res state pre t0 dur relay_ok
  offer=$(cr_offer "$dst" "$code" "$minor" "cr $tag $code $minor")
  [ -n "$offer" ] || { say "$tag: dst $dst issued no offer"; return 1; }
  case "$offer" in lno1*) : ;; *) say "$tag: not a BOLT12 offer: $(echo "$offer" | head -c 40)"; return 1 ;; esac
  rng=$(exp_range "$code" "$minor"); ref=$(echo "$rng" | cut -d' ' -f1)
  exp_min=$(echo "$rng" | cut -d' ' -f2); exp_max=$(echo "$rng" | cut -d' ' -f3)
  body="{\"invoice_str\":\"$offer\"}"
  [ -n "$explicit" ] && body="{\"invoice_str\":\"$offer\",\"amount\":$explicit}"
  t0=$(date +%s)
  res=$(rpc "$(API "$src")" pay "$body")
  state=$(echo "$res" | jqf 'd.get("state","")')
  pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  dur=$(( $(date +%s) - t0 ))
  relay_ok=0
  # Structural routing: no edge-edge channel exists, so an edge->edge
  # success necessarily forwarded r. relay_ok is topology, not log prose.
  if [ "$state" = "Success" ] && [ -n "$pre" ]; then relay_ok=1; fi
  echo "$(date +%FT%T),$tag,$src,$dst,currency-$code,$code,$minor,$ref/$exp_min/$exp_max,$state,${pre:0:16},$dur,$relay_ok" >> "$CSV"
  [ "$state" = "Success" ] && [ -n "$pre" ]
}

cr_pay_invoice() { # $1 tag $2 src $3 dst $4 msat (bolt11 baseline via r)
  local tag=$1 src=$2 dst=$3 msat=$4 inv res state pre t0 dur relay_ok
  inv=$(rpc "$(API "$dst")" invoice "{\"amount_msat\":$msat,\"description\":\"cr $tag baseline\"}" \
    | jqf 'd.get("bolt11","")')
  [ -n "$inv" ] || { say "$tag: dst $dst issued no invoice"; return 1; }
  t0=$(date +%s)
  res=$(rpc "$(API "$src")" pay "{\"invoice_str\":\"$inv\"}")
  state=$(echo "$res" | jqf 'd.get("state","")')
  pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
  dur=$(( $(date +%s) - t0 ))
  relay_ok=0
  if [ "$state" = "Success" ] && [ -n "$pre" ]; then relay_ok=1; fi
  echo "$(date +%FT%T),$tag,$src,$dst,bolt11,-,-,$msat,$state,${pre:0:16},$dur,$relay_ok" >> "$CSV"
  [ "$state" = "Success" ] && [ -n "$pre" ]
}

cr_probe() { # $1 src $2 dst — readiness/settle probe, quiet unless failing
  cr_pay_invoice "probe-$1-$2" "$1" "$2" 5000 >/dev/null 2>&1
}

pick_dst() { # $1 src -> an edge != src (seeded)
  local src=$1 cands=() e
  for e in "${EDGES[@]}"; do [ "$e" != "$src" ] && cands+=("$e"); done
  rand_pick "$2" "${cands[@]}"
}

cr_open() { # $1 from-name $2 to-name $3 to-id — open_channel that also
  # recognizes lampo's FLAT {"code","message"} error envelope (lib.sh only
  # checks nested error.message, so a failed fundchannel looks like success).
  local from=$1 to=$2 id=$3 resp sz
  rpc "$(API "$from")" connect "{\"node_id\":\"$id\",\"addr\":\"127.0.0.1\",\"port\":$(P2P "$to")}" >/dev/null
  resp=$(TMO=150 rpc "$(API "$from")" fundchannel \
    "{\"node_id\":\"$id\",\"addr\":\"127.0.0.1\",\"port\":$(P2P "$to"),\"amount\":$CR_CHANNEL_AMT_SATS,\"public\":true,\"push_msat\":$(( CR_CHANNEL_AMT_SATS * 500 ))}")
  case "$resp" in
    \{*) : ;;
    *) say "open $from->$to non-JSON: $(echo "$resp" | head -c 200)"; return 1 ;;
  esac
  if echo "$resp" | grep -q '"code"'; then
    say "open $from->$to RPC error: $(echo "$resp" | head -c 300)"; return 1
  fi
  for _ in $(seq 1 20); do   # funding tx in mempool BEFORE mining (race lesson)
    sz=$(bcli getmempoolinfo | jqf 'd["result"]["size"]'); [ "${sz:-0}" -gt 0 ] 2>/dev/null && break; sleep 3
  done
  mine 8
}

# ============================ main ====================================
mkdir -p "$CRDIR"
: > "$LOG"
echo "ts,tag,src,dst,method,currency,minor_units,exp_ref_min_max,state,preimage16,dur_s,relay_ok" > "$CSV"
say "currency-routing soak: bin=$BIN rounds=$CR_ROUNDS seed=$SEED rates=$CURRENCY_RATES tol_bps=$CURRENCY_TOLERANCE_BPS"
[ -x "$BIN" ] || { say "binary missing: $BIN"; exit 1; }
bcli getblockchaininfo | jqf 'd["result"]["chain"]' | grep -q regtest \
  || { say "bitcoind at $CORE_URL not regtest"; exit 1; }
GUARD_PIDFILE="$CRDIR/harness.pid"
if [ -f "$GUARD_PIDFILE" ]; then
  gp=$(cat "$GUARD_PIDFILE" 2>/dev/null)
  if [ -n "$gp" ] && [ -d "/proc/$gp" ] && grep -qa "currency_routing" "/proc/$gp/cmdline" 2>/dev/null; then
    say "another currency_routing harness live (pid $gp)"; exit 1
  fi
fi
echo $$ > "$GUARD_PIDFILE"

# Leftover nodes from a previous run would hold the ports; scoped pkill only
# (node dirs live in $SIMDIR per lib.sh node_dir; names r,e1..e5 are unique
# to this harness).
for n in r "${EDGES[@]}"; do pkill -9 -f "lampod-cli --data-dir $SIMDIR/$n " 2>/dev/null; done
sleep 2

say "phase 1: launch router + 5 edges"
for n in r "${EDGES[@]}"; do start_node "$n"; done
for n in r "${EDGES[@]}"; do ID[$n]=$(wait_up "$n") || fail "$n never came up"; done
say "  r=${ID[r]:0:16}… e1=${ID[e1]:0:16}… e2=${ID[e2]:0:16}…"

if cr_probe e1 e2; then
  say "phase 2: existing star already routes — resuming soak"
  # Restarted daemons re-sync gossip; BOLT12 legs need the announcer tick.
  sleep 120
else
  say "phase 2: fund edges + open star (edge -> r only, push half for return flow)"
  bal=$(bcli getbalance | jqf 'd["result"]' 2>/dev/null || echo 0)
  if ! python3 -c "exit(0 if float('${bal:-0}') > 50 else 1)" 2>/dev/null; then
    say "maturing regtest coins (101 blocks) for funding"
    mine 101
  fi
  for e in "${EDGES[@]}"; do fund_node "$e" 2 || fail "fund $e"; done
  # Edge wallets sync on a ~2-min cadence: opening channels before the funds
  # are visible fails with "Insufficient funds". Wait for the money first.
  say "waiting for edge wallets to see funds"
  for _ in $(seq 1 20); do
    total=0
    for e in "${EDGES[@]}"; do
      f=$(rpc "$(API "$e")" funds \
        | jqf 'sum(int(t["amount_msat"]) for t in d.get("transactions",[]) if int(t.get("amount_msat",0))>0)' 2>/dev/null)
      total=$(( total + ${f:-0} ))
    done
    [ "$total" -gt $(( CR_CHANNEL_AMT_SATS * 1000 * 5 )) ] && break
    sleep 15
  done
  [ "$total" -gt $(( CR_CHANNEL_AMT_SATS * 1000 * 5 )) ] || fail "edge funds never visible (total=${total:-0} msat)"
  for e in "${EDGES[@]}"; do
    cr_open "$e" r "${ID[r]}" || fail "open $e->r"
  done
  # Readiness is a payment probe, not log prose. Announcer needs ~60s ticks
  # before BOLT12 offers resolve; currency legs additionally sleep 150s below.
  for _ in $(seq 1 20); do
    ready=$(ready_channels e1 2>/dev/null || echo 0)
    [ "${ready:-0}" -ge 1 ] 2>/dev/null && break
    sleep 15
  done
  for _ in $(seq 1 40); do cr_probe e1 e2 && break; sleep 15; done
  cr_probe e1 e2 || fail "star never became routable"
  say "phase 2b: announcer settle before first currency offer"
  sleep 150
fi
say "star ready: all edge-edge volume forwards r"

say "phase 3: fixed currency assertions (ring covers every edge as src and dst)"
cr_pay_currency c1-usd-ring e1 e2 USD 250 \
  || fail "c1 USD 250c e1->e2 via r"
cr_pay_currency c2-eur-ring e2 e3 EUR 100 \
  || fail "c2 EUR 100c e2->e3 via r"
cr_pay_currency c3-usd-ring e3 e4 USD 175 \
  || fail "c3 USD 175c e3->e4 via r"
cr_pay_currency c4-eur-ring e4 e5 EUR 220 \
  || fail "c4 EUR 220c e4->e5 via r"
cr_pay_currency c5-usd-ring e5 e1 USD 90 \
  || fail "c5 USD 90c e5->e1 via r"
# Boundary: explicit amount at the accepted max must succeed...
rng=$(exp_range USD 250); exp_max=$(echo "$rng" | cut -d' ' -f3)
cr_pay_currency c6-at-max e1 e2 USD 250 "$exp_max" \
  || fail "c6 explicit max $exp_max msat rejected"
# ...and above it must fail locally (no payment attempted).
if cr_pay_currency c7-over-max e1 e2 USD 250 $(( exp_max + 50000 )); then
  fail "c7 over-tolerance explicit amount unexpectedly paid"
else
  say "c7 over-tolerance explicit amount refused as expected"
  echo "$(date +%FT%T),c7-over-max,e1,e2,currency-USD,USD,250,refused,ExpectedFail,-,0,0" >> "$CSV"
fi
# Unknown currency must be refused at offer build (UnsupportedCurrency).
if [ -n "$(cr_offer e2 GBP 100 'c8 unknown code')" ]; then
  fail "c8 GBP offer unexpectedly built"
else
  say "c8 unknown currency refused as expected"
  echo "$(date +%FT%T),c8-gbp,e1,e2,currency-GBP,GBP,100,refused,ExpectedFail,-,0,0" >> "$CSV"
fi
cr_pay_invoice c9-bolt11-fwd e2 e5 1000000 \
  || fail "c9 bolt11 baseline e2->e5 via r"
cr_pay_invoice c10-bolt11-rev e5 e3 1000000 \
  || fail "c10 bolt11 baseline e5->e3 via r"

say "phase 4: seeded stress ($CR_ROUNDS rounds, chaos every $CHAOS_EVERY)"
r=0
while :; do
  r=$((r+1))
  src=$(rand_pick "cr-$r-src" "${EDGES[@]}")
  dst=$(pick_dst "$src" "cr-$r-dst")
  m=$(rand_pick "cr-$r-m" cur-usd cur-usd cur-eur cur-eur invoice offer-msat)
  minor=$(( 100 + $(rand0 "cr-$r-minor" 4900) ))
  marks=$(log_marks)
  case "$m" in
    cur-usd) cr_pay_currency "s$r-usd" "$src" "$dst" USD "$minor" \
      || fail "stress round $r $src->$dst USD ${minor}c" ;;
    cur-eur) cr_pay_currency "s$r-eur" "$src" "$dst" EUR "$minor" \
      || fail "stress round $r $src->$dst EUR ${minor}c" ;;
    invoice) cr_pay_invoice "s$r-inv" "$src" "$dst" "$(rand_amount "cr-$r" 10000 5000000)" \
      || fail "stress round $r $src->$dst bolt11" ;;
    offer-msat)
      offer=$(rpc "$(API "$dst")" offer "{\"description\":\"cr s$r msat\",\"amount_msat\":400000}" \
        | jqf 'd.get("bolt12","")')
      [ -n "$offer" ] || fail "stress round $r $dst issued no msat offer"
      t0=$(date +%s)
      res=$(rpc "$(API "$src")" pay "{\"invoice_str\":\"$offer\"}")
      state=$(echo "$res" | jqf 'd.get("state","")')
      pre=$(echo "$res" | jqf 'd.get("payment_preimage") or ""')
      dur=$(( $(date +%s) - t0 ))
      rok=0; if [ "$state" = "Success" ] && [ -n "$pre" ]; then rok=1; fi
      echo "$(date +%FT%T),s$r-msat,$src,$dst,offer-msat,-,-,400000,$state,${pre:0:16},$dur,$rok" >> "$CSV"
      [ "$state" = "Success" ] && [ -n "$pre" ] \
        || fail "stress round $r $src->$dst msat offer" ;;
  esac
  health_scan_since "$marks" || fail "health scan after round $r"
  if [ "$CHAOS_EVERY" != 0 ] && [ $(( r % CHAOS_EVERY )) = 0 ]; then
    say "chaos: bouncing router r after round $r"
    bounce_marks=$(log_marks)
    kill9 r; sleep 2
    start_node r
    rid=$(wait_up r) || fail "router r never came back"
    [ "$rid" = "${ID[r]}" ] || fail "router came back with a DIFFERENT node_id"
    sleep 10
    # Settle before the next pay round: probe, not prose.
    cr_probe e1 e2 || fail "settle probe failed after router bounce"
    cr_pay_currency "bounce-$r" e5 e4 EUR 150 \
      || fail "post-bounce currency payment"
    health_scan_since "$bounce_marks" || fail "health scan after router bounce"
  fi
  [ "$CR_ROUNDS" != 0 ] && [ "$r" -ge "$CR_ROUNDS" ] && break
done

say "phase 5: edge coverage gate (every edge sends AND receives)"
if ! python3 - "$CSV" <<'EOF'; then
import csv,sys
srcs,dsts=set(),set()
with open(sys.argv[1]) as f:
    for row in csv.DictReader(f):
        if row['state']=='Success' and row['src'].startswith('e'):
            srcs.add(row['src']); dsts.add(row['dst'])
need={'e1','e2','e3','e4','e5'}
missing=need-srcs|need-dsts
print('src:',sorted(srcs),'dst:',sorted(dsts))
sys.exit(1 if missing else 0)
EOF
  fail "edge coverage incomplete"
fi
say "edge coverage OK"

say "CURRENCY-ROUTING COMPLETE: $r stress rounds + 10 fixed legs OK, results in $CSV"
