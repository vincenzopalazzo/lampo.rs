# Plug the Validating Lightning Signer (VLS) into lampo through a signer interface

## Clarified Problem Statement

**Goal:** Make the Lightning signer a replaceable module in lampo, so that VLS
(https://gitlab.com/lightning-signer/validating-lightning-signer) can back channel
and node signing without lampo holding the channel keys.

**Where lampo stands today (the "interface-first" claim does not yet hold for the signer):**

- `WalletManager::ldk_keys()` returns the concrete `Arc<LampoKeys>`
  (`lampo-common/src/wallet.rs`). The wallet owns the signer and derives it from the BDK
  xprv (`lampo-bdk-wallet/src/lib.rs:100`).
- `LampoKeysManager` (`lampo-common/src/keys.rs`) wraps LDK `KeysManager` and pins
  `type EcdsaSigner = InMemorySigner`. It also implements `OutputSpender` and
  `ChangeDestinationSource`, routing destination/shutdown scripts to the wallet via
  `set_wallet`.
- `Arc<LampoKeysManager>` and `InMemorySigner` are hard-coded in every LDK type alias
  (`lampo-common/src/types.rs`: `LampoChainMonitor`, `LampoArcChannelManager`,
  `LampoSweeper`, `LampoRouter`) and in `lampod/src/ln/channel_manager.rs`,
  `peer_manager.rs`, `offchain_manager.rs`, `actions/handler.rs` (`BumpHandler`).
  `get_channel_monitors` returns `ChannelMonitor<InMemorySigner>`.
- The only existing "alternative signer" is the `unsafe_channel_keys` feature, which
  is a branch inside `derive_channel_signer`, not a second implementation.

**Where VLS stands today (v1.0.0-rc.1, 2026-09-03):**

- `vls-core` pins `lightning = "0.2.4"`. Lampo is on `lightning = "0.3.0-rc2"`. Any crate
  that links `vls-core` (`vls-protocol-client`, `vls-proxy`, `vls-frontend`) brings a
  second `lightning` major into the graph; its `SignerProvider`/`ChannelSigner` impls
  are for different trait types and cannot be handed to lampo's `ChannelManager`.
  **This is the hard blocker for a direct dependency right now.**
- `vls-protocol` (the CLN-hsmd wire messages) depends only on `bitcoin`, `serde_bolt`,
  `txoo`, `bolt-derive`. No `lightning` dep, so it can coexist with LDK 0.3.
- Ready-made LDK adapters: in-process `LoopbackSignerKeysInterface` /
  `LoopbackChannelSigner` (`vls-core/src/util/loopback.rs`) and remote
  `KeysManagerClient` / `SignerClient` over a sync `Transport` trait
  (`vls-protocol-client/src/lib.rs`), plus the type-erasers `DynSigner` /
  `DynKeysInterface` (`vls-protocol-client/src/dyn_signer.rs`). Reference LDK node is
  `lnrod` (itself on LDK 0.1, behind VLS main).
- Gaps relative to what lampo uses: neither adapter implements
  `ChangeDestinationSource`; loopback lacks `OutputSpender`; the remote `SignerClient`
  has `todo!()` for `sign_justice_revoked_output`, `sign_justice_revoked_htlc`,
  `sign_counterparty_htlc_transaction` (breach handling would panic), and both have
  `todo!()` for splicing/keyed-anchor methods.
- VLS needs its own persistence (`Persist` via `vls-persist` KVV/redb) and a chain
  feed (`vls-frontend` `ChainFollower`: headers + TXOO proofs + heartbeats), and
  policy setup (`SimpleValidatorFactory`, allowlist of `XPub`/`Script`/`Payee`).
- All node-side signer calls are synchronous and blocking, matching LDK's sync
  `SignerProvider`. Only the chain-feed side is async.

**Constraints:**

- No `lightning` version split in lampo's dependency graph. `deny.toml` and the
  LDK-group Dependabot bumps assume one version (see the `[workspace.dependencies]`
  comment about issue #537).
- Existing channels must keep deriving the same keys (`KeysManager::new(.., false)`
  v1 derivation comment in `keys.rs`). The default in-memory signer must remain
  byte-for-byte the current behavior.
- Destination, shutdown and change scripts must keep coming from the on-chain wallet
  (`next_wallet_script`), so sweeps land in the BDK wallet. With VLS this means the
  wallet xpub must be registered in the signer allowlist, or closes get rejected.
- `LampoDaemon` must stay embeddable (`lampo-c-ffi`, `lampo-lang-bind`); no tokio
  runtime requirement inside signer calls.
- New dependencies need maintainer sign-off (CLAUDE.md). VLS crates would be
  optional behind a feature.
- Keep it simple: no signer plugin registry, no dynamic loading.

**Non-goals:**

- Moving the on-chain (BDK) wallet keys into VLS. First cut: VLS holds node + channel
  keys, BDK stays a hot wallet for L1. Full watch-only L1 is a follow-up.
- Supporting the serial/embedded (STM32) VLS transport.
- Implementing VLS policy tuning UI/RPCs beyond a config path to an allowlist file.
- Splicing support via VLS (VLS itself has `todo!()` there).

**Success criteria:**

- `lampo-common` exposes a signer abstraction; no LDK type alias and no `lampod`
  manager names `LampoKeysManager` or `InMemorySigner`. The wallet may still
  construct the default `LampoKeysManager` from its seed.
- A second implementation exists in-tree and is exercised by tests (at minimum the
  deterministic `unsafe_channel_keys` signer becomes its own impl instead of an
  `if` branch).
- `make check` and the integration suite pass unchanged with the default signer.
- Later, with an out-of-process VLS signer (Approach B: `vlsd` behind
  `remote_hsmd_socket`, no shared `lightning` version needed), a regtest node opens a
  channel, pays, force-closes and sweeps with `vlsd` holding the keys, and a policy
  violation (e.g. close to a non-allowlisted address) is rejected by the signer, not
  by lampo. Not a criterion for the seam itself.

## Approaches Considered

### Approach A: Signer seam first (type-erase the signer in lampo, no VLS dep yet)
- Sketch: Add `lampo-common/src/signer.rs` with `trait LampoSigner: EntropySource +
  NodeSigner + SignerProvider<EcdsaSigner = LampoChannelSigner> + OutputSpender +
  ChangeDestinationSourceSync + Send + Sync` and a `LampoChannelSigner(Box<dyn
  ErasedChannelSigner>)` newtype forwarding `ChannelSigner`/`EcdsaChannelSigner`
  (LDK 0.3 puts no `Writeable` bound on `EcdsaSigner`, verified in
  `lightning-0.3.0-rc2/src/sign/mod.rs:1100`, so a boxed signer is legal; monitors
  re-derive signers through `derive_channel_signer` on read). The async
  `ChangeDestinationSource` returns `impl Future` and is not object safe, so it cannot
  be a supertrait; `OutputSweeper`'s change-destination slot gets a small
  `LampoChangeDestination(Arc<dyn LampoSigner>)` adapter instead. Keep
  `LampoKeysManager` in `keys.rs` as the default impl. Replace the entropy, node
  signer, signer provider and output spender slots in the type aliases and managers
  with `Arc<dyn LampoSigner>`. Make `WalletManager::ldk_keys()` return
  `Arc<dyn LampoSigner>` and add the signer as a `LampoDaemon` constructor input with
  the wallet's signer as the default. Mirror VLS's `DynSigner`/`DynKeysInterface`
  shape so a later adapter plugs in with a thin wrapper.
- Affected files: `lampo-common/src/{keys.rs,types.rs,wallet.rs,lib.rs}`,
  `lampod/src/lib.rs`, `lampod/src/ln/{channel_manager,peer_manager,offchain_manager}.rs`,
  `lampod/src/actions/handler.rs`, `lampo-bdk-wallet/src/lib.rs`,
  `lampo-chain/src/lib.rs` (test mock), `lampo-testing/src/lib.rs`.
- Tradeoffs: pure lampo work, unblocked today, makes the "modular" claim true for the
  signer and pays off for any HSM, not only VLS. One extra vtable hop per signing
  call (negligible next to network IO). Does not deliver VLS itself.
- Effort: M.

### Approach B: Lampo-owned hsmd wire client (`lampo-vls`), out of process
- Sketch: New crate `lampo-vls` implementing the Approach A trait by speaking the
  CLN-hsmd protocol from `vls-protocol` (no `lightning` dep, so no version clash) over
  a UNIX socket to `remote_hsmd_socket`, which already bundles the chain frontend and
  the gRPC leg to `vlsd`. This is the exact deployment shape CLN uses, so no chain
  feed or persistence code lands in lampo. Lampo re-implements the mapping from LDK
  trait calls to hsmd messages (what `vls-protocol-client` does today, roughly 1.5k
  lines) but against LDK 0.3.
- Affected files: new `lampo-vls/`, `lampod-cli/src/main.rs` (select signer from
  `LampoConf`), `lampo-common/src/conf.rs` (`signer`, `vls_socket`, allowlist path),
  docker/simulation setup for `vlsd` + `remote_hsmd_socket`.
- Tradeoffs: independent of the VLS LDK bump, and the process boundary is the
  security property you actually want from VLS. Cost is duplicating and then tracking
  `vls-protocol-client` semantics (channel setup ordering, `dbid`/`peer_id` mapping,
  the `todo!()` gaps become lampo's to fill). Divergence risk from upstream.
- Effort: L.

### Approach C: Upstream VLS to LDK 0.3, then reuse `vls-protocol-client` behind a feature
- Sketch: Contribute the `lightning 0.2.4 -> 0.3` bump to VLS (they bump per release:
  0.1 in 0.14.0, 0.2.4 now). Then add `lampo-vls` as a thin wrapper: implement
  `LampoSigner` for `KeysManagerClient` (adding the missing
  `ChangeDestinationSource` via the wallet, as `LampoKeysManager` does today), pick
  `NullTransport` (in-process `RootHandler`, keys still in-process but policy
  enforced) or `GrpcTransport` (keys in `vlsd`), and run `vls-frontend` /
  `SignerPortFront` from lampo's tokio runtime fed by the existing bitcoind config.
- Affected files: same as B on the lampo side, minus the wire client; plus a VLS
  merge request.
- Tradeoffs: least code in lampo and stays aligned with upstream. Blocked on VLS
  review cadence and on LDK 0.3 reaching a final release (lampo is on an rc). Pulls
  redb, tonic, prost into lampo's optional deps. Remote `SignerClient` still panics on
  justice/counterparty-HTLC signing until upstream fills those `todo!()`s.
- Effort: lampo side S-M (on top of A); upstream M-L.

## Recommendation

**Decision (2026-09-30): do Approach A now. Approach C is off the table** (an upstream
VLS bump to LDK 0.3 is not going to happen on lampo's timeline). Approach B is the
eventual VLS route: it is the only one that avoids the `lightning` version clash
without upstream, and A is its prerequisite. Design the seam so B is a new crate
implementing `LampoSigner`, with no further changes to the LDK type aliases or the
channel-manager generics; B still needs daemon and CLI injection (`with_signer`,
signer selection and socket/allowlist config in `LampoConf`).

Scope of A, concretely:

1. `lampo-common/src/signer.rs`: `trait LampoSigner` (EntropySource + NodeSigner +
   SignerProvider<EcdsaSigner = LampoChannelSigner> + OutputSpender +
   ChangeDestinationSourceSync + Send + Sync), the boxed `LampoChannelSigner`, and
   the `LampoChangeDestination` adapter for the sweeper's async slot.
2. `LampoKeysManager` in `keys.rs` implements it and stays the default.
3. Replace `Arc<LampoKeysManager>` / `InMemorySigner` in `types.rs` and the four
   `lampod` consumers with `Arc<dyn LampoSigner>` / `LampoChannelSigner`; the
   sweeper's change destination becomes `Arc<LampoChangeDestination>`.
4. Signer becomes a `LampoDaemon` constructor input, defaulting to the wallet's.
5. Second in-tree impl for tests (fold `unsafe_channel_keys` into it or delete it).

## Open questions

- **Ownership of the seed.** Should `LampoDaemon::new` grow a signer argument, or
  should `WalletManager` keep vending it? With VLS the wallet and the signer have
  different seeds, so the daemon-argument form is cleaner. Inferred: daemon argument
  with `wallet.ldk_keys()` as the default. Needs your call.
- **Which VLS mode is the target?** Inferred: `vlsd` over gRPC (keys out of process)
  is the goal; in-process loopback is a stepping stone/test mode only.
- **L1 wallet with VLS.** Is a hot BDK wallet next to a cold channel signer acceptable
  for the first cut, or is the whole point cold L1 too? Affects whether
  `sign_psbt`/`spend_spendable_outputs` must route through VLS `sign_onchain_tx`.
- **Deterministic test keys.** Is `unsafe_channel_keys` still used by anything? Only
  `lampo-bdk-wallet/Cargo.toml` references it. If dead, drop it during A instead of
  porting it.
- **Splicing/keyed anchors.** VLS has `todo!()` there. If lampo plans to enable
  splicing before VLS catches up, the VLS signer must be documented as incompatible.

