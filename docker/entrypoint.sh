#!/bin/bash
# LAMPO_CHAIN_BACKEND=core (default) or rbitcoin. The workflow matrix sets it.
export LAMPO_CHAIN_BACKEND="${LAMPO_CHAIN_BACKEND:-core}"
echo "chain backend: ${LAMPO_CHAIN_BACKEND}"
make check TEST_LOG_LEVEL='debug'
