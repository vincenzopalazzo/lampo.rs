#!/usr/bin/env bash
#
# Guard the unified-chain-sync invariant: the on-chain wallet crate
# (`lampo-bdk-wallet`) must NOT depend directly on any LDK (`lightning*`) crate.
#
# The LDK <-> wallet coupling lives in `lampo-chain` via the
# `WalletChainListener` adapter, so the wallet stays independently replaceable.
# See docs/designs/unified-chain-sync.md.
#
# Note: a *transitive* `lightning` dep is expected (through `lampo-common`); we
# only forbid a *direct* dependency, hence `--depth 1`.
set -euo pipefail

direct=$(cargo tree -p lampo-bdk-wallet --edges normal --depth 1 --prefix none 2>/dev/null \
    | grep -iE 'lightning' || true)

if [ -n "$direct" ]; then
    echo "::error::lampo-bdk-wallet directly depends on an LDK crate:" >&2
    echo "$direct" >&2
    echo "Keep the on-chain wallet free of LDK chain-sync; put the coupling in" >&2
    echo "lampo-chain instead (see docs/designs/unified-chain-sync.md)." >&2
    exit 1
fi

echo "OK: lampo-bdk-wallet has no direct LDK (lightning*) dependency."
