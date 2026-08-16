#!/usr/bin/env bash
# Smoke-test lampo's LND-compatible REST API the way Zeus does.
#
# Official `lncli` is gRPC-only and cannot validate a REST-only listener.
# This script mirrors the LND docs curl examples:
#   https://docs.lightning.engineering/lightning-network-tools/lnd/macaroons
#
# Usage:
#   ./tools/lnd-rest-smoke.sh [host] [port] [macaroon_path]
#
# Defaults:
#   host=127.0.0.1 port=7979
#   macaroon_path=$LAMPO_DIR/<network>/lnd-rest/macaroons/admin.macaroon
set -euo pipefail

HOST="${1:-127.0.0.1}"
PORT="${2:-7979}"
MACAROON_PATH="${3:-}"

if [[ -z "${MACAROON_PATH}" ]]; then
  if [[ -z "${LAMPO_DIR:-}" ]]; then
    echo "Provide macaroon path or set LAMPO_DIR" >&2
    exit 1
  fi
  NETWORK="${LAMPO_NETWORK:-testnet}"
  MACAROON_PATH="${LAMPO_DIR}/${NETWORK}/lnd-rest/macaroons/admin.macaroon"
fi

if [[ ! -f "${MACAROON_PATH}" ]]; then
  echo "macaroon not found: ${MACAROON_PATH}" >&2
  exit 1
fi

MACAROON_HEX="$(xxd -p -c 100000 "${MACAROON_PATH}" | tr -d '\n')"
BASE="https://${HOST}:${PORT}"

echo "==> GET ${BASE}/v1/getinfo"
curl -sk \
  --header "Grpc-Metadata-macaroon: ${MACAROON_HEX}" \
  "${BASE}/v1/getinfo" | python3 -m json.tool

echo
echo "==> GET ${BASE}/v1/balance/blockchain"
curl -sk \
  --header "Grpc-Metadata-macaroon: ${MACAROON_HEX}" \
  "${BASE}/v1/balance/blockchain" | python3 -m json.tool

echo
echo "==> GET ${BASE}/v1/channels"
curl -sk \
  --header "Grpc-Metadata-macaroon: ${MACAROON_HEX}" \
  "${BASE}/v1/channels" | python3 -m json.tool

echo
echo "OK: LND REST smoke passed"
