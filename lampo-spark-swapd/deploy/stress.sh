#!/usr/bin/env bash
#
# Chaos driver: run the real swap end-to-end suite against the local
# operator stack in a loop, injecting a fault between every round, and
# stop the moment a round fails -- that failure IS the crash we are
# hunting. Coins are worthless (regtest / signet), so this is safe to
# hammer.
#
# What each round proves (the suite's own assertions): a Direction A
# swap claims the counterparty's spark htlc with the preimage the
# lightning payment revealed; a Direction B swap keeps the lightning
# payment held until the spark htlc is claimed; a crashed Direction A
# claim recovers to Done unaided. Between rounds we perturb the system
# -- restart an operator, reorg bitcoind, stall a container -- so the
# next round runs against an operator set and chain that just survived a
# fault. A real bug shows up as a round that no longer settles.
#
# Usage: deploy/stress.sh [rounds]   (default 25)
#
# Assumptions (see RUNBOOK.md): the stack is up under compose project
# `spark` (so containers are spark-bitcoind-1, spark-spark-operator-N-1),
# /tmp/spark-tls/ca.crt exists, and this runs from the crate dir with
# cargo + PROTOC on PATH.
set -uo pipefail

ROUNDS="${1:-25}"
CRATE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BCLI=(docker exec spark-bitcoind-1 bitcoin-cli -regtest -rpcuser=testutil -rpcpassword=testutilpassword -rpcport=8332)
OPERATORS=(spark-spark-operator-0-1 spark-spark-operator-1-1 spark-spark-operator-2-1)
LOG="/tmp/swapd-stress-$(date +%s).log"

say() { echo "[stress $(date +%H:%M:%S)] $*" | tee -a "$LOG"; }

run_suite() {
  # The real swaps. Serialized: they share one bitcoind and operator set.
  ( cd "$CRATE_DIR" && TEST_LOG_LEVEL=off cargo test --release --test spark_regtest -- \
      --ignored --test-threads=1 ) >>"$LOG" 2>&1
}

# --- fault injectors: each must leave the system able to serve swaps ---

fault_restart_operator() {
  local op="${OPERATORS[$((RANDOM % ${#OPERATORS[@]}))]}"
  say "fault: restart $op"
  docker restart "$op" >>"$LOG" 2>&1
  sleep 8   # let it re-register with the pool
}

fault_reorg() {
  # Invalidate the tip and mine past it: a 1-block reorg under the
  # operators' feet. Exercises the chain watcher and htlc maturity logic.
  local tip depth=2
  tip=$("${BCLI[@]}" getbestblockhash 2>/dev/null) || return 0
  say "fault: ${depth}-block reorg from $tip"
  local h="$tip"
  for _ in $(seq 1 "$depth"); do
    local prev; prev=$("${BCLI[@]}" getblockheader "$h" 2>/dev/null | grep -o '"previousblockhash": *"[0-9a-f]*"' | grep -o '[0-9a-f]\{64\}')
    [ -n "$prev" ] && h="$prev"
  done
  "${BCLI[@]}" invalidateblock "$h" >>"$LOG" 2>&1
  local addr; addr=$("${BCLI[@]}" getnewaddress 2>/dev/null)
  "${BCLI[@]}" generatetoaddress $((depth + 2)) "$addr" >>"$LOG" 2>&1
  sleep 3
}

fault_stall_operator() {
  # Freeze an operator briefly, then thaw: a slow/unresponsive signer.
  local op="${OPERATORS[$((RANDOM % ${#OPERATORS[@]}))]}"
  say "fault: stall $op for 5s"
  docker pause "$op" >>"$LOG" 2>&1
  sleep 5
  docker unpause "$op" >>"$LOG" 2>&1
  sleep 3
}

FAULTS=(fault_restart_operator fault_reorg fault_stall_operator)

say "starting: $ROUNDS rounds, log $LOG"
survived=0
for r in $(seq 1 "$ROUNDS"); do
  say "round $r/$ROUNDS: running swap suite"
  if run_suite; then
    survived=$r
    say "round $r: OK"
  else
    say "round $r: SUITE FAILED -- crashed it. Last 60 log lines:"
    tail -60 "$LOG"
    say "SUMMARY: survived $((r-1)) clean rounds before failure. Full log: $LOG"
    exit 1
  fi
  # perturb before the next round
  f="${FAULTS[$((RANDOM % ${#FAULTS[@]}))]}"
  "$f"
done

say "SUMMARY: all $survived rounds passed under fault injection. Log: $LOG"
