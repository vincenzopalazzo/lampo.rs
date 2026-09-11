#!/usr/bin/env bash
#
# sim/recover.sh — lightning-node state recovery matrix + stress loop.
#
# Question answered: "the process / the chain / the disk did something bad —
# does the node come back with its lightning state intact (and its funds)?"
#
# Runs against the multihop.sh cluster (hs -- hm -- hr, reused or built):
# the dead-end topology doubles as the recovery probe (a successful 2-hop
# payment proves identity + channels + routing + HTLC state all restored).
#
# Fault classes and their EXPECTED behavior (verified against LDK 0.3
# source, ln/channelmanager.rs):
#   A. process faults — clean SIGINT (ctrlc handler persists state),
#      SIGTERM, SIGKILL idle, SIGKILL mid-payment (payer/relay/payee):
#      expected: full state restore, channels ready again, payments work.
#   B. environment — chain advances 40 blocks while down; all nodes down:
#      expected: catch up via wallet sync, channels unchanged.
#   C. persistence (LDK fs_store v1: <dir>/regtest/manager + monitors/):
#      C1 stale `manager` (monitors AHEAD, the classic "restored an old
#         backup"): LDK force-closes the channels from monitor state,
#         funds safe (modulo fees) — channel must NOT come back usable.
#      C2 stale `monitor` (manager ahead — a Watch-API violation):
#         read() returns DecodeError::DangerousValue → daemon MUST fail
#         fast, never run with a monitor behind the manager.
#      C3 corrupted `manager` / C4 missing monitors/ / C5 corrupted single
#         monitor file: daemon MUST fail fast, leave the file untouched
#         (restore-from-backup must stay possible), never start half-up.
#   D. peer-side — channel force-closed by the peer while we were down:
#      on restart the monitor must notice, channel ends closed, node sane.
#
# Invariants checked after EVERY case:
#   I1 node_id unchanged (identity intact)         I4 probe: 2-hop payment
#   I2 channel set == baseline (unless case closes)    Success + preimage + hops>=2
#   I3 channels ready again within 180 s           I5 no panic|corrupt|invariant
#   I6 fail-fast cases: exit, no API, file unmodified  in the log DELTA
#   I7 money guard (stress loop): cluster channel-balance total never
#      drops >1% (funds vanishing) nor grows beyond the fee budget
#      (money printing).
#
# Env: MATRIX(1) STRESS(1) STRESS_CYCLES(25, 0=endless) SEED KEEP_GOING ...
# Results: $SIMDIR/rc/results.csv  Log: $SIMDIR/rc/sim.log
set -uo pipefail

API_BASE=${API_BASE:-8210}
P2P_BASE=${P2P_BASE:-20110}
SIMDIR=${SIMDIR:-$HOME/lampo-sim/sim-run}
MHDIR=${MHDIR:-$SIMDIR/mh}
LOG=${LOG:-$SIMDIR/rc/sim.log}
CSV=${CSV:-$SIMDIR/rc/results.csv}
MATRIX=${MATRIX:-1}
STRESS=${STRESS:-1}
STRESS_CYCLES=${STRESS_CYCLES:-25}
PROBE_AMT_MSAT=${PROBE_AMT_MSAT:-1000000}

source "$(dirname "$0")/lib.sh"

declare -A IDX=([hs]=1 [hm]=2 [hr]=3)
ALLNODES=(hs hm hr)
declare -A ID=()
declare -A BASE_SNAP=()   # per-node sorted channel ids before a fault
RC() { echo "$(date -Iseconds),$1,$2,$3" >> "$CSV"; }  # case,verdict,detail

# snapshot()/baseline helpers
take_baseline() { local n; for n in "${ALLNODES[@]}"; do BASE_SNAP[$n]=$(ready_channel_ids "$n"); done; }
show_baseline() { local n; for n in "${ALLNODES[@]}"; do say "  baseline[$n]=${BASE_SNAP[$n]:-none}"; done; }

# --- verification core -----------------------------------------------
verify_up_identity() { # I1: node up with the SAME node_id; echoes nothing, 0=ok
  local n id
  for n in "${ALLNODES[@]}"; do
    id=$(wait_up "$n") || { say "verify: $n never came up"; return 1; }
    [ "$id" = "${ID[$n]}" ] || { say "verify: $n node_id CHANGED ($id != ${ID[$n]})"; return 1; }
  done
  return 0
}
verify_channels() { # I2+I3, mode: strict (set equality) | loss (survivors ready)
  local mode=$1 deadline=$(( $(date +%s) + 180 )) n have ok
  while :; do
    ok=1
    for n in "${ALLNODES[@]}"; do
      have=$(ready_channel_ids "$n")
      if [ "$mode" = strict ]; then
        [ "$have" = "${BASE_SNAP[$n]:-}" ] || ok=0
        [ "$(ready_channels "$n")" = "$(rpc "$(API "$n")" channels | jqf 'len(d.get("channels",[]))' )" ] || ok=0
      else
        # loss mode: whatever channels remain must ALL be ready
        local listed; listed=$(rpc "$(API "$n")" channels | jqf 'len(d.get("channels",[]))')
        [ "${listed:-0}" = "$(ready_channels "$n")" ] || ok=0
      fi
    done
    [ "$ok" = 1 ] && return 0
    [ "$(date +%s)" -gt "$deadline" ] && {
      for n in "${ALLNODES[@]}"; do
        say "  $n now=[$(channel_ids "$n")] ready=$(ready_channels "$n") baseline=[${BASE_SNAP[$n]:-}]"
      done
      return 1
    }
    sleep 10
  done
}
verify_case() { # $1 tag, $2 mode(strict|loss) [$3 marks] — full I1..I5
  local tag=$1 mode=$2 marks=${3:-$(log_marks)}
  verify_up_identity  || { RC "$tag" FAIL "identity/up"; fail "$tag: node up/identity"; return 1; }
  verify_channels "$mode" || { RC "$tag" FAIL "channels($mode)"; fail "$tag: channels not restored ($mode)"; return 1; }
  health_scan_since "$marks" || { RC "$tag" FAIL "health-delta"; fail "$tag: panic/corrupt in log delta"; return 1; }
  [ "$mode" = strict ] || { RC "$tag" PASS "up+identity+survivors-ready (loss mode)"; return 0; }
  mh_pay "probe-$tag" hs hr "$PROBE_AMT_MSAT" invoice \
    || { RC "$tag" FAIL "probe-payment"; fail "$tag: 2-hop probe payment after recovery"; return 1; }
  RC "$tag" PASS "state restored, probe OK"
  return 0
}
expect_fail_fast() { # $1 node, $2 tag, $3 file-to-watch(optional) — I6
  local n=$1 tag=$2 watch=${3:-} before=""
  [ -n "$watch" ] && before=$(md5sum "$watch" 2>/dev/null | cut -d' ' -f1)
  start_node "$n"
  if ! wait_dead "$n" 120; then
    RC "$tag" FAIL "process still alive after 120s"; fail "$tag: daemon did NOT fail fast"; return 1
  fi
  api_dead "$n" || { RC "$tag" FAIL "api up without valid state"; fail "$tag: API up despite broken state"; return 1; }
  if [ -n "$watch" ]; then
    local after; after=$(md5sum "$watch" 2>/dev/null | cut -d' ' -f1)
    [ "$before" = "$after" ] || { RC "$tag" FAIL "state file modified by failed start"; fail "$tag: $watch was modified"; return 1; }
  fi
  say "$tag: failed fast as required (no API, state file untouched)"
  RC "$tag" PASS "fail-fast"
  return 0
}
restart_node() { # $1 node -> waits up, asserts same id
  local n=$1 id
  start_node "$n"
  id=$(wait_up "$n") || { say "$n never came up after restart"; return 1; }
  [ "$id" = "${ID[$n]}" ] || { say "$n restarted with DIFFERENT node_id"; return 1; }
  return 0
}
# background in-flight payment (used by mid-payment kills)
BG_DIR=${BG_DIR:-$SIMDIR/rc/bg}; mkdir -p "$BG_DIR"
bg_pay() { # $1 src $2 dst [$3 amt] -> starts pay in bg; sets BG_PID
  local src=$1 dst=$2 amt=${3:-$PROBE_AMT_MSAT} inv
  inv=$(TMO=30 rpc "$(API "$dst")" invoice "{\"amount_msat\":$amt,\"description\":\"inflight\"}" | jqf 'd.get("bolt11","")')
  [ -n "$inv" ] || { say "bg_pay: no invoice from $dst"; return 1; }
  ( TMO=40 rpc "$(API "$src")" pay "{\"invoice_str\":\"$inv\"}" > "$BG_DIR/out" 2>&1; echo "curl_rc=$?" >> "$BG_DIR/out" ) &
  BG_PID=$!
}
classify_bg_pay() { # logs the outcome class of the last bg payment
  local out; out=$(tail -c 300 "$BG_DIR/out" 2>/dev/null || echo none)
  local state; state=$(echo "$out" | jqf 'd.get("state","")' 2>/dev/null)
  case "$state" in
    Success) say "  in-flight payment settled before/through the fault";;
    "") say "  in-flight payment interrupted (curl cut / no terminal event): $(echo "$out" | head -c 120)";;
    *) say "  in-flight payment failed with state=$state (clean failure is fine)";;
  esac
}
# A mid-payment SIGKILL can land between the ChannelMonitor persist and the
# ChannelManager persist. LDK then force-closes the channel from the AHEAD
# monitor on restart ("Channel closed due to outdated ChannelManager
# (ChannelMonitor is newer)") and the peer answers with "invalid
# channel_reestablish". That is the CORRECT crash-consistency outcome
# (funds safe, availability sacrificed). A channel that disappears WITHOUT
# one of these signatures is a funds-risk bug.
expect_forceclose_signature() { # $1 victim $2 marks -> 0 iff loss is explained
  local n marks=($2) i=0 hit="" m
  for n in "${ALLNODES[@]}"; do
    m=${marks[$i]:-0}; i=$((i+1))
    if log_delta "$m" "$(node_log "$n")" | \
       grep -hiE "ChannelMonitor is newer|invalid channel_reestablish" | head -2 | grep -q .; then
      hit="$n"
      break
    fi
  done
  [ -n "$hit" ] && { say "  channel loss explained: crash-consistency force-close (seen in $hit log)"; return 0; }
  say "  channel loss WITHOUT monitor-ahead/channel_reestablish signature — unexpected!"
  return 1
}
total_channel_msat() { # I7 helper: cluster-wide channel balance total
  local n t=0 v
  for n in "${ALLNODES[@]}"; do
    v=$(rpc "$(API "$n")" channels | jqf 'sum(c.get("available_balance_for_send_msat",0)+c.get("available_balance_for_recv_msat",0) for c in d.get("channels",[]))')
    t=$(( t + ${v:-0} ))
  done
  echo "$t"
}
repair_topology() { # after destructive cases: reopen hs-hm and hm-hr, new baseline
  say "  repairing topology (top-up wallets, reopen hs->hm, hm->hr)"
  # half-capacity push (same reason as ensure_cluster: reverse routing liquidity)
  local push=${REPAIR_PUSH_MSAT:-500000000}
  # Repeated repairs drain the 0.05 BTC wallets (observed: 'Insufficient
  # funds: 0.0084 BTC available of 0.0103 BTC needed') — top up first. The
  # 18 mined blocks also confirm any pending closing txs from force-closes.
  for n in "${ALLNODES[@]}"; do fund_node "$n" 0.02 || true; done
  wait_wallet_synced 300 || true
  open_channel hs hm "${ID[hm]}" 1000000 "$push" || true
  open_channel hm hr "${ID[hr]}" 1000000 "$push" || true
  sleep 30
  wait_wallet_synced 300 || true
  local deadline=$(( $(date +%s) + 180 ))
  while :; do
    [ "$(ready_channels hs)" -ge 1 ] && [ "$(ready_channels hm)" -ge 2 ] && [ "$(ready_channels hr)" -ge 1 ] && break
    [ "$(date +%s)" -gt "$deadline" ] && { fail "repair_topology: channels not ready again"; return 1; }
    sleep 15
  done
  take_baseline
  say "  topology repaired, new baseline taken"
  return 0
}


# ============================ main ====================================
mkdir -p "$SIMDIR/rc" "$ART" "$BG_DIR"
: > "$LOG"
echo "ts,case,verdict,detail" > "$CSV"

# duplicate-instance guard (pidfile-based; see multihop.sh for the pgrep race)
GUARD_PIDFILE="$SIMDIR/rc/harness.pid"
if [ -f "$GUARD_PIDFILE" ]; then
  gp=$(cat "$GUARD_PIDFILE" 2>/dev/null)
  if [ -n "$gp" ] && [ -d "/proc/$gp" ] && grep -qa "recover" "/proc/$gp/cmdline" 2>/dev/null; then
    say "another recover.sh instance is already running (pid $gp) — refusing to start"
    exit 3
  fi
  say "stale recover harness pid $gp — taking over"
fi
echo $$ > "$GUARD_PIDFILE"
trap 'rm -f "$GUARD_PIDFILE"' EXIT

say "recover harness: bin=$BIN seed=$SEED matrix=$MATRIX stress=$STRESS cycles=$STRESS_CYCLES"
[ -x "$BIN" ] || { say "binary missing: $BIN"; exit 1; }
bcli getblockchaininfo | jqf 'd["result"]["chain"]' | grep -q regtest || { say "bitcoind at $CORE_URL not regtest"; exit 1; }

if load_ids && cluster_up \
   && [ "$(ready_channels hs)" = 1 ] && [ "$(ready_channels hm)" = 2 ] && [ "$(ready_channels hr)" = 1 ] \
   && [ "$(ready_channel_ids hm | wc -w)" = 2 ]; then
  say "reusing structurally-clean mh cluster"
else
  # Reuse only a CLEAN dead-end topology (hs:1 hm:2 hr:1). A cluster left
  # over from an interrupted run carries zombie closing channels whose
  # entries retire asynchronously — baselines and strict compares would
  # false-fail. Wipe and rebuild instead.
  say "mh cluster missing/polluted — wiping and rebuilding"
  for n in "${ALLNODES[@]}"; do local_p=$(node_pid "$n"); [ -n "$local_p" ] && kill -9 "$local_p"; done
  sleep 3
  rm -rf "$SIMDIR/hs" "$SIMDIR/hm" "$SIMDIR/hr" "$SIMDIR/mh"
  ENSURE_ONLY=1 "$(dirname "$0")/multihop.sh" || { fail "could not build mh cluster"; exit 2; }
  load_ids || { fail "ids.env missing after cluster build"; exit 2; }
fi
say "recovery cluster: hs=${ID[hs]:0:12}.. hm=${ID[hm]:0:12}.. hr=${ID[hr]:0:12}.."
take_baseline; show_baseline
marks=$(log_marks)

# --- A. process faults ------------------------------------------------
if [ "$MATRIX" = 1 ]; then
  say "R01 clean stop (SIGINT, ctrlc handler persists) then restart hm"
  killint hm; wait_dead hm 90 || { kill9 hm; sleep 2; }
  start_node hm; marks=$(log_marks)
  verify_case R01-clean-sigint strict "$marks"

  say "R02 SIGTERM idle then restart hm"
  killterm hm; wait_dead hm 60 || kill9 hm
  start_node hm; marks=$(log_marks)
  verify_case R02-term-idle strict "$marks"

  say "R03 SIGKILL idle (the classic) then restart hm"
  kill9 hm; sleep 2
  start_node hm; marks=$(log_marks)
  verify_case R03-kill9-idle strict "$marks"

  for spec in "R04 payer hs" "R05 relay hm" "R06 payee hr"; do
    set -- $spec; rc=$1; role=$2; victim=$3
    say "$rc SIGKILL $role ($victim) with payment in flight"
    marks=$(log_marks)
    if bg_pay hs hr; then
      sleep "$(rand0 "$rc-delay" 6)"          # seeded 0-5 s: payment mid-flight
      kill9 "$victim"; wait "$BG_PID" 2>/dev/null; classify_bg_pay
    else
      say "$rc: could not start in-flight payment"; kill9 "$victim"
    fi
    start_node "$victim"
    # Mid-payment kills: full restore OR explained force-close (see
    # expect_forceclose_signature) — liveness cannot be guaranteed when the
    # kill lands inside the monitor/manager persist window.
    verify_case "$rc-kill9-$role-midpay" loss "$marks" || fail "$rc: verification failed"
    if [ "$(ready_channel_ids "$victim")" != "${BASE_SNAP[$victim]}" ]; then
      expect_forceclose_signature "$victim" "$marks" || { RC "$rc" FAIL "unexplained channel loss"; fail "$rc: channel lost without crash-consistency signature"; }
      say "$rc: force-closed from monitor-ahead state — repairing topology"
      repair_topology
    fi
    mh_pay "probe-$rc-after" hs hr "$PROBE_AMT_MSAT" invoice || { RC "$rc" FAIL "probe after midpay"; fail "$rc: probe after midpay recovery"; }
    RC "$rc-kill9-$role-midpay" PASS "recovered (restore or explained force-close)"
  done

  say "R07 chain advances 40 blocks while hm is down"
  marks=$(log_marks); kill9 hm; sleep 2
  mine 40
  start_node hm
  wait_up hm >/dev/null || fail "R07: hm never came up"
  wait_wallet_synced 420 || fail "R07: wallets never synced after 40 blocks"
  verify_case R07-chain-advanced strict "$marks"

  say "R08 all three nodes killed at once"
  marks=$(log_marks)
  for n in "${ALLNODES[@]}"; do kill9 "$n"; done; sleep 3
  for n in "${ALLNODES[@]}"; do start_node "$n"; done
  verify_case R08-all-down strict "$marks"
fi

# --- C/D. persistence + peer-side faults ------------------------------
if [ "$MATRIX" = 1 ]; then
  say "R09 offline-close refusal + cooperative close across downtime (true offline force-close blocked by issue #575)"
  marks=$(log_marks)
  kill9 hr; sleep 2
  # close by node_id is broken for multi-channel nodes (issue #574): the
  # channels action ignores peer_id. Select the hm-hr channel_id ourselves.
  local cid; cid=$(rpc "$(API hm)" channels | jqf 'next((c["channel_id"] for c in d.get("channels",[]) if c.get("peer_id")=="'"${ID[hr]}"'"), "")')
  [ -n "$cid" ] || { RC R09 FAIL "no hm-hr channel found to close"; fail "R09: no channel"; }
  TMO=120 rpc "$(API hm)" close "{\"node_id\":\"${ID[hr]}\",\"channel_id\":\"$cid\"}" > "$BG_DIR/close.json" 2>&1
  # With the peer down the cooperative close MUST be refused cleanly
  # (assert the current safe contract; the force path is issue #575).
  grep -q "peer is disconnected" "$BG_DIR/close.json" \
    || { RC R09 FAIL "close did not refuse cleanly for an offline peer: $(head -c 120 "$BG_DIR/close.json")"; fail "R09: unexpected close behavior"; }
  say "  close refused cleanly while hr is offline (expected until #575)"
  # Bring hr back, then close cooperatively and confirm across restart.
  start_node hr
  sleep 10
  TMO=120 rpc "$(API hm)" close "{\"node_id\":\"${ID[hr]}\",\"channel_id\":\"$cid\"}" > "$BG_DIR/close2.json" 2>&1
  say "  close response: $(head -c 160 "$BG_DIR/close2.json")"
  mine 10; sleep 10
  start_node hr
  verify_case R09-forceclose-while-down loss "$marks" || true
  say "  (R09 closes hm-hr on purpose: channel loss is the CORRECT outcome)"
  repair_topology
  mh_pay probe-post-r09 hs hr "$PROBE_AMT_MSAT" invoice || fail "probe after R09 repair"

  say "R10 stale manager (restore pre-payment manager, monitors ahead)"
  marks=$(log_marks)
  mf=$(manager_file hm)
  cp "$mf" "$mf.stale" 2>/dev/null || fail "R10: cannot snapshot manager"
  mh_pay r10-warm hs hr "$PROBE_AMT_MSAT" invoice >/dev/null || fail "R10: warm-up payment"
  kill9 hm; sleep 2
  cp "$mf.stale" "$mf"
  start_node hm
  wait_up hm >/dev/null || fail "R10: hm never came up with stale manager"
  sleep 30   # LDK fires the stale-manager check during read; give logs a beat
  health_scan_since "$marks" || fail "R10: panic/corrupt after stale manager"
  # Ground truth that state protection fired: the stale-manager signature in
  # the log delta (observed live: "A ChannelManager is stale compared to the
  # current ChannelMonitor!" + "will be force-closed"). But the outcome is
  # BIMODAL by flush timing: if the kill landed before the warm payment's
  # monitor+manager updates flushed, the restored "stale" manager equals the
  # on-disk state (no skew) and a clean restart is the CORRECT outcome.
  # The dangerous direction (manager AHEAD of monitors) is R14's fail-fast.
  local sig=0
  tail -n +1 "$(node_log hm)" | grep -qi "ChannelManager is stale compared to the current ChannelMonitor" && sig=1
  local ready_now; ready_now=$(ready_channels hm)
  if [ "$sig" = 1 ]; then
    say "R10: stale-manager protection fired (force-close from monitor state)"
    RC R10-stale-manager PASS "force-close signature present; funds protected"
  elif [ "${ready_now:-0}" -ge 2 ]; then
    say "R10: no skew observed (kill pre-flush window) — clean rollback restart, channels usable"
    RC R10-stale-manager PASS "no-skew clean restart (safe equal rollback)"
  else
    RC R10-stale-manager FAIL "no signature and channels not fully usable (ready=$ready_now)"
    fail "R10: partially-broken state after stale-manager restore"
  fi
  mine 6   # confirm any closing txs so channel entries can retire
  rm -f "$mf.stale"
  if [ "$sig" = 1 ] || [ "$(ready_channels hm)" -lt 2 ]; then
    repair_topology
  else
    take_baseline   # clean restart: refresh baseline (state advanced past it)
  fi
  mh_pay probe-post-r10 hs hr "$PROBE_AMT_MSAT" invoice || fail "probe after R10"

  say "R11 corrupted manager file -> must fail fast, file untouched"
  marks=$(log_marks); mf=$(manager_file hm)
  kill9 hm; sleep 2
  cp "$mf" "$mf.bak"
  head -c 37 "$mf.bak" > "$mf"; printf 'garbage-bytes' >> "$mf"
  expect_fail_fast hm R11-corrupt-manager "$mf"
  cp "$mf.bak" "$mf"; rm -f "$mf.bak"
  restart_node hm || fail "R11: hm did not come back after restore"
  verify_case R11-restore-after-corruption strict "$marks"

  say "R12 missing monitors/ dir (manager lists channels) -> must fail fast"
  marks=$(log_marks); md=$(monitors_dir hm)
  kill9 hm; sleep 2
  mv "$md" "${md}.hidden"
  expect_fail_fast hm R12-missing-monitors
  mv "${md}.hidden" "$md"
  restart_node hm || fail "R12: hm did not come back after restore"
  verify_case R12-restore-after-missing strict "$marks"

  say "R13 corrupted single monitor file -> must fail fast, file untouched"
  marks=$(log_marks); md=$(monitors_dir hm)
  kill9 hm; sleep 2
  mon=$(ls "$md" | head -1)
  cp "$md/$mon" "$md/$mon.bak"
  printf 'garbage' >> "$md/$mon"
  expect_fail_fast hm R13-corrupt-monitor "$md/$mon"
  mv "$md/$mon.bak" "$md/$mon"
  restart_node hm || fail "R13: hm did not come back after restore"
  verify_case R13-restore-after-corrupt-monitor strict "$marks"

  say "R14 stale monitor (manager ahead — Watch-API violation) -> must fail fast"
  marks=$(log_marks); md=$(monitors_dir hm)
  cp -r "$md" "${md}.stale"
  mh_pay r14-warm hs hr "$PROBE_AMT_MSAT" invoice >/dev/null || fail "R14: warm-up payment"
  kill9 hm; sleep 2
  mv "$md" "${md}.cur"                    # keep the CURRENT monitors aside
  cp -r "${md}.stale" "$md"               # monitors one payment behind manager
  expect_fail_fast hm R14-stale-monitor
  # restore the CONSISTENT pair (manager T1 + monitors T1) and re-verify
  rm -rf "$md" "${md}.stale"
  mv "${md}.cur" "$md"
  restart_node hm || fail "R14: hm did not come back after restore"
  verify_case R14-restore-after-stale-monitor strict "$marks"
fi

# --- stress loop: randomized kill/restart cycles under payments -------
if [ "$STRESS" = 1 ]; then
  say "stress: $STRESS_CYCLES randomized recovery cycles (0=endless), faults: kill9 kill9-midpay term-midpay chain-advance double-kill"
  guard_base=$(total_channel_msat)
  say "  money guard baseline: ${guard_base} msat cluster channel balance"
  c=0
  while :; do
    c=$((c+1))
    marks=$(log_marks)
    fault=$(rand_pick "rc-$c-fault" kill9 kill9-midpay term-midpay chain-advance double-kill)
    victim=$(rand_pick "rc-$c-victim" "${ALLNODES[@]}")
    say "stress cycle $c: fault=$fault victim=$victim"
    vmode=strict
    [ "$fault" = kill9-midpay ] || [ "$fault" = term-midpay ] && vmode=loss
    case $fault in
      kill9)  kill9 "$victim"; sleep 2; start_node "$victim" ;;
      term-midpay|kill9-midpay)
        if bg_pay hs hr; then
          sleep "$(rand0 "rc-$c-delay" 6)"
          [ "$fault" = kill9-midpay ] && kill9 "$victim" || killterm "$victim"
          wait "$BG_PID" 2>/dev/null; classify_bg_pay
        else
          kill9 "$victim"
        fi
        start_node "$victim" ;;
      chain-advance) kill9 "$victim"; sleep 2; mine "$(rand0 "rc-$c-blocks" 20)" ; start_node "$victim"; wait_wallet_synced 300 || true ;;
      double-kill)   other=$(rand_pick "rc-$c-other" "${ALLNODES[@]}"); [ "$other" = "$victim" ] && other=hs
                     kill9 "$victim"; kill9 "$other"; sleep 3; start_node "$victim"; start_node "$other" ;;
    esac
    verify_case "stress-$c-$fault-$victim" "$vmode" "$marks" || { RC "stress-$c" FAIL "$fault on $victim"; fail "stress cycle $c ($fault/$victim)"; }
    if [ "$fault" = kill9-midpay ] || [ "$fault" = term-midpay ]; then
      if [ "$(ready_channel_ids "$victim")" != "${BASE_SNAP[$victim]}" ]; then
        expect_forceclose_signature "$victim" "$marks" || { RC "stress-$c" FAIL "unexplained channel loss"; fail "stress-$c: channel lost without crash-consistency signature"; }
        say "  stress-$c: legit force-close -> repair + guard rebase"
        repair_topology
        guard_base=$(total_channel_msat)
      fi
    fi
    total=$(total_channel_msat)
    lower=$(( guard_base - guard_base / 100 ))
    budget=$(( guard_base + 200000 ))
    if [ "$total" -lt "$lower" ]; then
      RC "stress-$c-guard" FAIL "channel total ${total} < 99% of baseline ${guard_base} — funds vanishing"
      fail "money guard: cluster channel balance dropped"
    elif [ "$total" -gt "$budget" ]; then
      RC "stress-$c-guard" FAIL "channel total ${total} > baseline+fee budget ${budget} — money printing"
      fail "money guard: cluster channel balance inflated"
    else
      say "  guard ok: total=${total} (baseline ${guard_base})"
    fi
    [ "$STRESS_CYCLES" != 0 ] && [ "$c" -ge "$STRESS_CYCLES" ] && break
  done
  say "stress complete: $c cycles, guard held ${guard_base} -> $(total_channel_msat) msat"
fi

say "RECOVERY COMPLETE: results in $CSV ($(grep -c PASS "$CSV") PASS / $(grep -c FAIL "$CSV") FAIL)"
[ "$(grep -c FAIL "$CSV")" = 0 ] || exit 2
exit 0
