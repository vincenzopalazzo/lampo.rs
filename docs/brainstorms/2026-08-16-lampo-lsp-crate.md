# Brainstorm: lampo-lsp crate (run lampo as an LSP via lightning-liquidity)

Date: 2026-08-16

## Clarified Problem Statement

**Goal:** Add a new `lampo-lsp` workspace crate that integrates LDK's
`lightning-liquidity` (0.3.0-beta1) so lampo can act as both an LSP
service (sell JIT channels / liquidity) and an LSP client (buy
liquidity), covering LSPS0/1/2/5, enabled at runtime via config.

**Decisions made (user):**

- Role: both service side and client side.
- Specs: all of LSPS1, LSPS2, LSPS5 (LSPS0 transport is implied), each
  landing in a separate self-contained commit.
- Packaging: new `lampo-lsp` workspace crate, toggled by a runtime
  config flag (not a cargo feature).
- Architecture: modular plugin, not an ldk-node style monolith.
  `lampod` core must NOT hard-depend on `lampo-lsp`; the LSP plugs in
  through generic extension points and is composed at the binary level
  (`lampod-cli`). ldk-node already exists for people who want the blob.
- Done when: integration test in `lampo-testing` where a client node
  obtains a JIT channel from a lampo LSP.

**Constraints:**

- The workspace pins every `lightning*` crate to `0.3.0-beta1` and bumps
  them together (issue #537); `lightning-liquidity = "0.3.0-beta1"` must
  join `[workspace.dependencies]` in the root `Cargo.toml`.
- Upstream marks service-side support "beta / not production ready" —
  the lampo config and docs must label the LSP service as experimental.
- Each commit must independently pass `make fmt` and `make check`
  (repo rule: no fixup commits).
- `lampo-bdk-wallet` must stay LDK-free (CI guard from commit
  `be284e5`); `lampo-lsp` must not leak into the wallet crate.
- New dependency needs maintainer sign-off per CLAUDE.md — but
  `lightning-liquidity` lives in the rust-lightning workspace itself,
  so it is effectively the same dependency family lampo already trusts.

**Non-goals:**

- No hand-rolled LSPS protocol implementation.
- No standalone LSP daemon/binary separate from `lampod`.
- No production hardening of the service side beyond what upstream
  offers (it is beta upstream).

**Success criteria:**

- `lampo-lsp` crate compiles as a workspace member; lampod boots with
  the LSP disabled by default and enabled via a `[lsp]` config section.
- LSPS0 messages round-trip through the `custom_message_handler` slot.
- Integration test in `lampo-testing`: client lampo node pays a
  LSPS2 JIT invoice and receives a just-in-time channel from a lampo
  LSP node on regtest.
- Client-side RPC commands exposed (e.g. `lsps2-buy`, `lsps1-order`)
  through the existing `ExternalHandler` mechanism.

## Key integration points found in the codebase

Lampo already has two of the three extension points a modular LSP
plugin needs:

- `lampo-common/src/event.rs`: `Event::RawLDK(ldk::events::Event)`
  flows raw LDK events through the generic `Emitter`/`Subscriber`
  pub/sub. `lampo-lsp` can subscribe to `HTLCIntercepted`,
  `ChannelReady`, and `PaymentForwarded` without lampod knowing it
  exists.
- `lampo-common/src/handler.rs`: `ExternalHandler` registry
  (`lampod/src/actions/handler.rs`) — the natural home for new LSP RPC
  methods without touching core dispatch.

The missing third extension point:

- `lampod/src/ln/peer_manager.rs`: `SimpleArcPeerManager` currently
  uses `IgnoringMessageHandler` for `custom_message_handler` (line
  118). `LiquidityManager` implements `CustomMessageHandler`, but
  LDK's `CustomMessageHandler: CustomMessageReader` carries an
  associated `CustomMessage` type, so the slot is not directly
  dyn-safe. A modular design needs a small type-erased custom-message
  router (see Approach B).
- LSPS2 service side additionally needs:
  - `UserConfig.accept_intercept_htlcs = true` on the ChannelManager.
  - Handling of `Event::HTLCIntercepted`, `Event::ChannelReady`, and
    `Event::PaymentForwarded` in lampod's event loop, forwarded to the
    `LSPS2ServiceHandler`.
  - A background task polling `LiquidityManager::next_event()`.
- `lightning-liquidity` 0.3 ships a `persist` module; hook it into
  lampod's persistence backend for service-side state.

## Approaches Considered

### Approach A: Concrete generic wiring (ldk-node style) — REJECTED

- Sketch: `lampod` depends on `lampo-lsp` and replaces
  `IgnoringMessageHandler` with `Arc<LampoLiquidityManager>` in the
  `SimpleArcPeerManager` type aliases; constructed no-op when the
  config flag is off. This is exactly how `ldk-node` integrates the
  crate.
- Tradeoffs: simplest types, but bakes LSP knowledge into lampod core
  and creates a hard `lampod -> lampo-lsp` dependency.
- Rejected (user decision): lampo should stay modular; ldk-node
  already exists as the monolithic option.

### Approach B: Plugin architecture with a type-erased message router

- Sketch: `lampo-lsp` is a self-contained plugin crate that hooks into
  lampod through three generic extension points, and lampod core never
  names any LSP type:
  1. **P2P custom messages (new infra):** lampod gains a
     `LampoCustomMessageRouter` — a single concrete type filling the
     `custom_message_handler` slot, holding
     `Vec<Arc<dyn LampoMsgHandler>>` registered at init, mirroring
     `ExternalHandler`. The erased trait works on raw bytes + message
     type number (LDK's associated `CustomMessage` type is satisfied
     once, inside the router, with a raw-bytes wire message
     implementing `Type` + `Writeable`). `lampo-lsp` provides an
     adapter that decodes to `RawLSPSMessage` (type 37913) and
     forwards to `LiquidityManager`'s handler, including
     `peer_connected`/`peer_disconnected`, feature bits, and draining
     `get_and_clear_pending_msg` back through the router.
  2. **Events (existing):** subscribe to `Event::RawLDK` for
     `HTLCIntercepted`/`ChannelReady`/`PaymentForwarded`, plus a
     background task polling `LiquidityManager::next_event()`.
  3. **RPC (existing):** LSP commands registered as `ExternalHandler`.
  Dependency direction: `lampo-lsp -> lampod` (it needs
  `LampoChannelManager` handles to construct `LiquidityManager`).
  Composition happens in `lampod-cli`, which reads the `[lsp]` config
  section and instantiates/registers the plugin — lampod core stays
  LSP-free.
- Affected files: `lampod/src/ln/peer_manager.rs` (swap
  `IgnoringMessageHandler` for the router), new
  `lampod/src/ln/msg_router.rs` (or lampo-common), new `lampo-lsp/`
  crate, `lampod-cli` (composition), `lampo-common/src/conf.rs`
  (`[lsp]` section), root `Cargo.toml`, `lampo-testing`.
- Tradeoffs: keeps lampod generic (the router benefits any future
  custom-message plugin, not just LSP); costs one encode/decode hop
  per custom message (negligible — LSPS0 messages are JSON strings
  anyway) and ~100 lines of erasure plumbing. One residual coupling:
  `UserConfig.accept_intercept_htlcs` must be set at ChannelManager
  construction, driven by the `[lsp]` config section in lampo-common.
- Effort: M for the router + wiring; L across all LSPS commits.

### Approach C: Generic type parameter with composition in lampod-cli

- Sketch: make `LampoPeerManager`/`LampoDaemon` generic over the
  custom message handler (default `IgnoringMessageHandler`), and let
  `lampod-cli` instantiate `LampoDaemon<LampoLiquidityManager>` when
  LSP is enabled.
- Tradeoffs: zero runtime indirection and no erasure code, but the
  generic parameter ripples through every concrete type alias in
  lampod (`InnerLampoPeerManager`, daemon fields, handler types) and
  monomorphizes two daemon variants; heavy type surgery for one plugin
  point.
- Effort: L, mostly mechanical generics churn.

### Approach D: Separate lampo-lspd binary

- Sketch: a new binary crate embedding `lampod` as a library and
  wiring the LiquidityManager only there; the main daemon stays
  untouched.
- Affected files: new `lampo-lspd/` crate mirroring `lampod-cli`.
- Tradeoffs: isolates experimental code completely; but duplicates
  runtime/config/deployment surface, and the user explicitly wants
  "run lampo as an LSP", not a second daemon.
- Effort: L.

## Recommendation

Approach B (plugin architecture with a type-erased message router).
It honors the modularity requirement — lampod core gains only a
generic custom-message router that mirrors the existing
`ExternalHandler` pattern, and everything LSP-specific lives in
`lampo-lsp` with composition in `lampod-cli`. Approach A was rejected
as an ldk-node style blob; Approach C achieves similar decoupling but
the generics churn across lampod's type aliases costs more than the
~100 lines of erasure code in B and monomorphizes two daemon
variants.

## Suggested commit sequence (each self-contained)

0. `lampod: add type-erased custom message router` — generic
   `LampoMsgHandler` trait + router in the `custom_message_handler`
   slot, no LSP knowledge; useful on its own for any plugin.
1. `lsp: add lampo-lsp crate with LiquidityManager wiring` — LSPS0
   transport via the router adapter, `[lsp]` config section,
   `Event::RawLDK` subscription, background event-processing task,
   composition in `lampod-cli`. Disabled by default.
2. `lsp: implement LSPS2 service side (JIT channels)` —
   `accept_intercept_htlcs`, HTLC interception events, JIT channel
   open flow, plus the lampo-testing integration test (the success
   criterion).
3. `lsp: implement LSPS2 client side` — buy-request RPC via
   `ExternalHandler`, httpd route.
4. `lsp: implement LSPS1 client and service` — channel ordering API.
5. `lsp: implement LSPS5 client and service` — webhook registration.

## Open questions (not blocking commit 1)

- LSPS2 service fee policy defaults: opening fee (min/proportional),
  max client-to-LSP payment size, channel size bounds — config knobs
  or hardcoded conservative defaults first?
- Where does service-side persistence live — reuse lampod's
  `persistence` backend or the `lightning-liquidity` persist module's
  own KVStore path?
- Should `lampo-httpd` expose LSPS endpoints publicly, or CLI/JSON-RPC
  only for the first iteration?
- LSPS5 webhooks need an outbound HTTP client on the service side —
  reuse whatever lampo-httpd already pulls in, or keep it minimal?
