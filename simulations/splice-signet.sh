#!/usr/bin/env bash
#
# simulations/splice-signet.sh — splice QA on the signet already running.
#
# Does not mine and does not stop other lampo processes. Ports and the
# data dir stay off the regtest splice run and off the soak tree.
# Fund with FAUCET_TOKEN, or let splice.sh pay the mutinynet L402.
#
# Usage:
#   REPO=$HOME/lampo-splice-qa BIN=$REPO/target/release/lampod-cli \
#     ./simulations/splice-signet.sh
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
export REPO=${REPO:-$HOME/lampo-splice-qa}
export BIN=${BIN:-$REPO/target/release/lampod-cli}
export SIMDIR=${SIMDIR:-$HOME/lampo-splice-qa/sim-run-signet}
export CORE_URL=${CORE_URL:-http://127.0.0.1:38332}
export API_BASE=${API_BASE:-8810}
export P2P_BASE=${P2P_BASE:-20810}
export CONFIRM_WAIT=${CONFIRM_WAIT:-1800}
export CORE_TMO=${CORE_TMO:-120}
export LAMPO_PLUGINS=${LAMPO_PLUGINS:-$(dirname "$BIN")/examples/lampo-bitcoind}

exec "$HERE/splice.sh"
