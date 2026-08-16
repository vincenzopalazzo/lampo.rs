# Plan: lampo-lsp plugin (Approach B)

**Goal:** Land a modular LSP plugin: lampod gains a type-erased custom-message
router with no LSP knowledge; a new `lampo-lsp` crate wraps
`lightning-liquidity` 0.3.0-beta1 and is composed at the binary layer
(`lampod-cli` / `lampo-testing`). First PR covers LSPS0 transport +
`lsp-info` / `lsps0-list-protocols` RPCs, proven by router unit tests, a
lampo-cli smoke test, and a two-node LSPS0 round-trip. LSPS1/2/5 JIT
channels are follow-up commits.

**Approach:** Approach B from
`docs/brainstorms/2026-08-16-lampo-lsp-crate.md`. Rejected A (ldk-node
blob: lampod would depend on lampo-lsp). Rejected C (generic
LampoDaemon parameter). Rejected D (second daemon).

## Affected files

- `lampo-common/src/msg.rs` — add `LampoMsgHandler` + `LampoWireMessage`
- `lampod/src/ln/msg_router.rs` — `LampoCustomMessageRouter` filling
  `custom_message_handler`
- `lampod/src/ln/peer_manager.rs` — swap `IgnoringMessageHandler` for the
  router; keep an `Arc` so plugins can register after `init`
- `lampod/src/lib.rs` — `add_custom_msg_handler`
- `lampo-common/src/conf.rs` + `lampo.example.conf` — `lsp-enable` /
  `lsp-client` / `lsp-service` / `lsp-advertise` (disabled by default;
  experimental)
- `lampo-lsp/` — new crate, depends on `lampod` + `lightning-liquidity`
- `lampod-cli/src/main.rs` — compose plugin before `HttpdHandler`
- `lampo-testing/src/lib.rs` — same composition; expose `api_url`
- `lampo-httpd` — `lsp-info` and `lsps0-list-protocols` routes that
  delegate to `LampoDaemon::call` (plugin registered first, no recurse)
- `tests/tests` — lampo-cli smoke + LSPS0 list_protocols
- root `Cargo.toml` — workspace member + `lightning-liquidity` pin

## Approach

Mirror `ExternalHandler`: a `Vec<Arc<dyn LampoMsgHandler>>` behind
`std::sync::RwLock`. The router is the single concrete
`CustomMessageHandler` (associated type `LampoWireMessage` = type id +
raw bytes). `lampo-lsp` adapts `LiquidityManager` / `RawLSPSMessage`
(type 37913). Composition lives in `lampod-cli`; lampod never names
LSP types.

Always attach the plugin. When `lsp-enable=false` it answers `lsp-info`
with `enabled: false` and claims no custom-message types.

Do **not** pass the LM into `process_events_async` in this PR (that
would type-couple lampod to lightning-liquidity). Persist from a
lampo-lsp background task. BG-processor wakeup is a follow-up.

Do **not** set `htlc_interception_flags` yet (LSPS2 follow-up; LDK 0.3
renamed `accept_intercept_htlcs`).

## Edge cases

- Handler list cloned before dispatch; never hold the `RwLock` across
  plugin code that could re-enter.
- `provided_*_features` / `peer_connected` evaluated at connect time, so
  register the plugin after `init` and before `listen`.
- HttpdHandler POSTs unknown methods; LSP ExternalHandler must be
  registered first and must return `Some` for `lsp-info` even when
  disabled, or HTTP `lsp-info` recurses.
- Invalid LSPS JSON: forward to LiquidityManager (it ignores the peer);
  do not re-parse JSON in the adapter.
- `lampo-bdk-wallet` stays LDK-free; do not add lampo-lsp there.
- Pin `lightning-liquidity = "0.3.0-beta1"` with the rest of the stack.

## Test plan

- Unit: router with no handlers returns `None` for unknown types;
  registered echo handler round-trips bytes; feature bits OR.
- Integration: default node `lampo-cli getinfo` still works; `lsp-info`
  reports disabled.
- Integration: two nodes with `lsp-enable` (client + service), connect,
  `lsps0-list-protocols` via lampo-cli returns a protocol list (empty is
  OK — LSPS1/2/5 not wired yet).
- `make fmt` + `make check`.

## Conventions to follow

- `unwrap` only with `// SAFETY:` / tests; `anyhow` via
  `lampo-common::error`; log `target:`; import groups std / external /
  crate.
- Commits: imperative, ≤50 chars, prefix `lampod:` / `lsp:`; each
  commit independently green; no fixups.
- DCO `-s`.

## Open questions / risks (deferred)

- LSPS2 fee policy and intercept event path (fund-loss if we enable
  intercept without forwarding `HTLCIntercepted`).
- Injecting LM into `process_events_async` without lampod depending on
  lightning-liquidity.
- Persistence namespace sharing with `FilesystemStore`.

**Estimated size:** L (>200 LOC) for the full sequence; this PR is the
router + LSPS0 slice (M–L).

## Follow-up PRs (not this one)

1. LSPS2 service (JIT) + intercept flags + lampo-testing JIT test
2. LSPS2 client RPC (`lsps2-buy`)
3. LSPS1 client/service
4. LSPS5 client/service
