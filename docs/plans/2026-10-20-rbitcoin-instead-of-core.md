# Experiment: rbitcoin instead of Bitcoin Core

**Date:** 2026-10-20
**Worktree:** `/Users/vincenzopalazzo/github/work/btc/lampo-workspace/lampo.rs-rbitcoin`
**Branch:** `exp/rbitcoin-regtest` at `a094e7f8` (`origin/main`)
**Binary:** rbitcoin v0.8.0 Apple Silicon, checksums matched `SHA256SUMS.darwin`
**Core:** Bitcoin Core v28.1.0 from the nix profile

No code was changed. This is a measurement, not a swap.

## Verdict

Do not replace Bitcoin Core with rbitcoin to make tests faster.

On the path tests actually take (empty regtest, mine ~100 blocks, talk JSON-RPC), rbitcoin is not a win. With `--sh-index` it is slower. Without it, mining 100 blocks is a few tenths of a second faster, and every fresh datadir still preallocates ~1 GB. The integration harness cannot point at it without a new miner backend: `LampoTesting` boots Core through `corepc-node` (`clightning-testing`) and funds wallets with `getnewaddress` plus `generatetoaddress`. rbitcoin has no wallet RPC.

## What lampo actually needs from Core

Two different clients:

1. **Test harness** (`lampo-testing`): `corepc-node` starts a regtest `bitcoind`, reads the cookie, then mines and funds via `bitcoincore_rpc` (`getnewaddress`, `generatetoaddress`, `getblockchaininfo`).
2. **Node chain sync** (`lampo-chain` via the `lampo-bitcoind` plugin): `getblockchaininfo`, `getblock`, `getblockheader`, `sendrawtransaction`, `estimatesmartfee`, `getmempoolinfo`. Wallet history is a separate BDK emitter on the same Core RPC.

rbitcoin v0.8.0 implements that second list, plus regtest `generatetoaddress` / `generateblock`. It does **not** implement `getnewaddress` or a watch-only wallet. Esplora is off unless `--esplora-listen` is set; address history needs `--sh-index` and fails closed without it. Docs say ldk-node Esplora channel watches work without the scripthash index, but the BDK wallet does not.

## Timings (this machine, empty regtest, no peers)

| Step | Bitcoin Core 28.1 | rbitcoin 0.8.0 + `--sh-index` | rbitcoin 0.8.0, no sh-index |
|---|---|---|---|
| Process up, RPC answers | 0.11s | ~2.3s (attempt 23 × 100ms) | ~0.8s |
| `generatetoaddress` 100 | **5.14s** | **8.19s** | **4.70s** |
| `getblock` verbosity 0 | <0.01s | 0.02s | n/a |
| `getblockheader` / `getmempoolinfo` | <0.01s | <0.01s | <0.01s |
| Fresh datadir size after 100 blocks | **18 MB** | **1.0 GB** | **1.0 GB** |

`size_on_disk` in `getblockchaininfo` was `1044451833` at height 0. That is the archive preallocating `tx.head` (`slots=33554432`), not 100 blocks of chain data. `--sh-index` on an empty regtest also merges 64 empty scripthash shards before the node is useful.

Core's 5.14s includes `createwallet`. The mine itself is the bulk. rbitcoin has nothing equivalent to time against that wallet step.

## Integration test

`cargo test -p tests --lib lampo_tests::decode_invoice` did not finish in 5 minutes, then the test binary run directly did not finish in 90 seconds. Two `lampo-bitcoind` plugin processes were reparented to init and left running after the test process died. That stall is in the current Core-backed harness (plugin listen on `127.0.0.1:0`), not in rbitcoin. It is not evidence that a backend swap would make the suite faster.

## Why a swap would not speed the suite up

- Almost every integration test pays for a Core process start plus a mine of 100+ blocks. rbitcoin's mine is the same order of magnitude, and the 1 GB prealloc is a per-test disk cost `corepc-node` does not have.
- Funding still needs a wallet. A swap would mean an external miner key plus raw coinbase, or keeping Core just for `getnewaddress`. Either way the harness gets more code, not less waiting.
- `estimatesmartfee` on rbitcoin is a 10-minute inclusion frontier, not Core's historical buckets. Fee tests would need a new expected shape.
- Regtest `initialblockdownload` stayed `true` at height 100 with no peers. Anything that waits on `!initialblockdownload` would hang.
- Mainnet IBD is the case rbitcoin is built for (compact archive, in-process Electrum/Esplora). This repo's tests and `simulations/` are regtest-only and must stay that way. A faster mainnet sync does not move `make check`.

## Matrix (landed)

`LAMPO_CHAIN_BACKEND` selects the miner `LampoTesting::tmp` starts:

- `core` (default, unset): Bitcoin Core via `corepc-node`, same as before.
- `rbitcoin`: `rbitcoin-node` from `LAMPO_RBITCOIN` or `PATH`. Cookie file, no wallet. Mining is `generatetoaddress` to the lampo address.

Integration workflow builds the test harness and `lampo-bitcoind` once, then runs that binary on Core 25.1 and rbitcoin 0.8.0 (`fail-fast: false`). Locally: `make integration` (Core) or `LAMPO_CHAIN=all make integration`. There is no Docker image.

rbitcoin is not a replacement. The lane exists so a regtest RPC break shows up in CI. Do not point `simulations/` at it.

## If someone still wants the experiment

Do not replace `BtcNode`. Add an optional backend behind `LampoTesting::new` only:

1. Start `rbitcoin-node --network regtest --no-listen --no-seeds --rpc --rpc-listen 127.0.0.1:0` **without** `--sh-index` unless a test hits address history.
2. Mine with `generatetoaddress` to a harness-owned regtest address. Build the coinbase credit yourself; do not call `getnewaddress`.
3. Point `lampo.conf` `core_url` at that RPC. Cookie file is `--rpc-cookie-file`; there is no `--rpcuser`.
4. Keep Core as the default. Gate the rbitcoin path on an env var so `make check` does not download a 15 MB binary or require a 1 GB tmp dir.

Stop there. Do not point `simulations/` at rbitcoin, and do not touch production / sacred nodes.
