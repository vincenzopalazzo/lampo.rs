# Brainstorm: rbitcoin chain plugin

**Goal:** Run Lampo's chain sync against an [rbitcoin](https://github.com/reardencode/rbitcoin) node through a plugin, the same way `lampo-bitcoind` fronts Bitcoin Core.

**Constraints:**

- Lampo already calls `getblockchaininfo`, `getblock` (verbosity 0), `getblockheader` (JSON and raw), `estimatesmartfee`, `getmempoolinfo`, and `sendrawtransaction` through the plugin dispatcher. Those methods exist on rbitcoin's Core-class JSON-RPC, with Core's result shapes (`loaded` / `mempoolminfee`, `feerate` in BTC/kvB).
- rbitcoin TCP auth is a bearer token in `{datadir}/rpc.token`, plus optional HTTP Basic from `--rpc-cookie-file`. The unix socket `{datadir}/rpc.sock` has no HTTP header. `lampo-bitcoind` only sends HTTP Basic from `core-user` / `core-pass`.
- The on-chain wallet still opens its own `bitcoincore_rpc` client. That client understands a cookie file or user/password, not a bearer token and not a unix socket. No new crates.
- `sendrawtransaction`'s `maxfeerate` on rbitcoin is sat/vB (default 10000). Lampo sends only the raw hex, so the default cap applies. Do not translate a Core BTC/kvB argument.

**Non-goals:**

- Esplora or Electrum as the Lampo chain backend. rbitcoin can serve both; this change is the JSON-RPC plugin.
- Replacing BDK's bitcoind emitter with the plugin.
- A Zig port. `lampo-bitcoind/zig` stays the Core plugin.
- Mainnet channel-fund guidance. rbitcoin's own Lightning doc still says not to put mainnet channel funds there until that file says the contract is boring.

**Success criteria:**

- `lampo-rbitcoin` starts as a plugin, reads `core-url` plus `rpc-token-file`, `rpc-cookie-file`, or `rpc-socket`, and registers the six chain methods.
- A cookie file is enough for both the plugin and the wallet. User and password stay optional in that case.
- Setting `rpc-socket` or `rpc-token-file` selects `lampo-rbitcoin` when the operator did not pass `--plugin`, once that binary is next to `lampod-cli` or under `examples/`.
- Unit tests cover endpoint selection, cookie splitting, and the HTTP response split. No live rbitcoin node in CI.

## Approaches Considered

### Approach A: Teach `lampo-bitcoind` the extra auth modes

- Sketch: extend the Core plugin with bearer, cookie file, and unix socket.
- Affected files: `plugin-sdk/rust/examples/bitcoind.rs`, `lampod-cli`.
- Tradeoffs: one binary, but Core and rbitcoin auth get mixed, and a Core cookie file would be ambiguous with rbitcoin's.
- Effort: S

### Approach B: New `lampo-rbitcoin` plugin, same RPC surface

- Sketch: a sibling of `lampo-bitcoind` that speaks rbitcoin's auth and proxies the methods Lampo already calls. `lampo.conf` gains `rpc-cookie-file`, `rpc-token-file`, and `rpc-socket`. The wallet uses `Auth::CookieFile` when a cookie path is set.
- Affected files: `plugin-sdk/rust/examples/rbitcoin.rs`, `lampo-common/src/conf.rs`, `lampo-chain/src/lib.rs`, `lampo-bdk-wallet/src/lib.rs`, `lampod-cli/src/main.rs`, `lampod-cli/src/args.rs`.
- Tradeoffs: a second plugin to build. The chain code does not grow a second backend. Wallet sync still needs TCP plus a cookie or user/password.
- Effort: M

### Approach C: Point Lampo at rbitcoin Esplora

- Sketch: use `--esplora-listen` for tip, blocks, and broadcast.
- Affected files: `lampo-chain` Esplora client, wallet backend selection.
- Tradeoffs: matches ldk-node's Esplora path, and address history needs `--sh-index`. It throws away the plugin dispatcher Lampo just grew for chain RPC.
- Effort: L

## Recommendation

Approach B. The plugin boundary is already the chain RPC seam, and rbitcoin's JSON-RPC subset is the Lightning path its own docs describe for LND polling and CLN `bcli`. Esplora can wait. Bearer-only TCP is enough for the plugin and not enough for the wallet; startup keeps requiring a cookie file or `core-user` / `core-pass`.

## Open questions

- Whether BDK should later sync through the plugin so a bearer token or unix socket is sufficient for the whole node. Not this change.
- `sendrawtransaction` over the 10000 sat/vB default. Lampo does not pass `maxfeerate`. Revisit if a real broadcast is rejected for the fee cap.
