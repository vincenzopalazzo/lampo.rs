# Lampo Production Fund-Safety

Date: 2026-08-09
Status: Brainstorm (clarified problem statement)

## Clarified Problem Statement

**Goal:** Make Lampo safe to run on mainnet with small funds by closing the
onchain fund-loss holes (sweeping, fees, broadcast, bumping), adding a
VSS-mirrored channel-state backup, unifying chain sync per the existing design
doc, and hardening startup/acceptance paths — reusing LDK's battle-tested
components (like ldk-node does) while keeping Lampo's own architecture.

**Constraints:**
- Reuse LDK off-the-shelf components: `lightning::util::sweep::OutputSweeper`,
  `BumpTransactionEventHandler`, `vss-client` — no Lampo-native rewrites.
- Filesystem stays the primary store (fast, fsync'd); VSS is a mirror/backup,
  not the primary KVStore. Recovery from VSS is an explicit tool/flow.
- Keep Lampo's design solid: small, self-contained PRs per CLAUDE.md; no
  dependency additions without maintainer sign-off (vss-client is the main new one).
- bitcoind RPC remains the only chain backend for now.
- LDK is pinned at 0.3 (git rev `84605cf`); all work targets that API.

**Non-goals:**
- Esplora/electrum/compact-filter backends.
- VSS as primary store (ldk-node's "VSS mode") — mirror only.
- Adopting ldk-node itself as a dependency; we mirror its patterns, not its crate.
- Watchtowers, splicing, LSP flows.
- Mnemonic encryption (worth doing, tracked separately — not fund-loss via LN).

**Success criteria:**
- Integration test: force-close a channel on regtest → `SpendableOutputs` is
  swept back into the BDK wallet automatically (sweeper survives restart mid-sweep).
- `FeeEstimator` serves per-`ConfirmationTarget` rates from `estimatesmartfee`
  (cached, with sane floors/fallbacks), no hardcoded 256 sat/kW anywhere.
- Anchor channels negotiable; `Event::BumpTransaction` handled end-to-end
  (CPFP a commitment tx on regtest under a fee spike).
- Broadcast failures are logged as errors, retried, and surfaced — no
  fire-and-forget `tokio::spawn` discarding results.
- Every KVStore write is mirrored to a running vss-server; a "restore from VSS
  onto empty datadir" drill recovers the node with monitors intact.
- Chain sync: single coordinator, each ChannelMonitor synced from its own best
  block (not the manager's); the signet 11-hour stall scenario is gone.
- Hardening: pid lock acquired before any state is read; inbound channels gated
  by a config policy (off by default on mainnet); corrupt graph/scorer reads
  warn loudly; `PaymentClaimable` no longer `unwrap()`s the preimage.

## Baseline findings (2026-08-09 audit)

Ranked fund-loss risks found in the code today:

1. `Event::SpendableOutputs` unhandled → force-close outputs never swept
   (`lampod/src/actions/handler.rs:457`; `None` sweeper at `lampod/src/lib.rs:290`).
2. `FeeEstimator` hardcoded to 256 sat/kW for every target
   (`lampod/src/chain/blockchain.rs:78-84`).
3. No `BumpTransactionEventHandler` / anchor support — no CPFP/RBF escape.
4. Broadcast errors discarded (`blockchain.rs:87-101`, `lampo-chain/src/lib.rs:126-139`).
5. ChainMonitor synced from ChannelManager's best block, not per-monitor
   (`lampo-chain/src/lib.rs:233-243`) — missed replay window after crash.
6. Persistence is `FilesystemStore` only; module doc itself says not
   production-ready (`lampod/src/persistence/mod.rs`).
7. Auto-accept of all inbound channels (`handler.rs:119-136`); pid lock taken
   after state load (`lampod-cli/src/main.rs:186`); silent fallback on corrupt
   graph/scorer; `preimage.unwrap()` panic path (`handler.rs:347`).

What's already sound: monitor persistence is synchronous and fsync'd
(FilesystemStore tmp→sync→rename→sync), and `docs/designs/unified-chain-sync.md`
already designs the sync fix.

## Approaches Considered

### Approach A: Safety-first phased ladder (onchain → VSS → sync unification)
- Sketch: Land the direct fund-loss fixes as a sequence of small PRs on the
  current architecture, gate mainnet on phases 1–2, and do the chain-sync
  redesign last. Phase 1 (mainnet gate): real fee estimator + broadcast retry +
  hardening batch. Phase 2 (mainnet gate): `OutputSweeper` wiring + anchors/
  `BumpTransactionEventHandler`. Phase 3: VSS mirror KVStore. Phase 4: unified
  chain sync per the design doc.
- Affected files: `lampod/src/chain/blockchain.rs` (FeeEstimator, broadcaster),
  `lampo-chain/src/lib.rs` (fee cache, broadcast result), `lampod/src/lib.rs`
  (sweeper wiring, change-destination source backed by BDK wallet),
  `lampod/src/actions/handler.rs` (events, accept policy, preimage),
  `lampod/src/persistence/mod.rs` (KVStore trait + mirror), `lampod-cli/src/main.rs`
  (pid lock), `lampo-common/src/conf.rs` (anchors, accept policy).
- Tradeoffs: fastest path to "mainnet with small funds"; each PR is small and
  testable. But the sweeper/fees land on top of the known-flaky dual sync
  pipeline, and some fee-estimator plumbing may be touched again in phase 4.
- Effort: L overall (Phase 1: S–M, Phase 2: M, Phase 3: M, Phase 4: L).

### Approach B: Architecture-first (unified sync coordinator as the foundation)
- Sketch: Implement `docs/designs/unified-chain-sync.md` first — single
  `ChainSyncCoordinator`, `impl Listen for BDKWalletManager`, per-listener best
  blocks — then build fee estimation, sweeper, and bump handling on the new
  coordinator, then VSS mirror.
- Affected files: new `lampo-chain` coordinator module, `lampo-bdk-wallet/src/lib.rs`
  (Listen impl, drop Emitter cron), then the same files as Approach A.
- Tradeoffs: cleanest end state, no rework, fixes the per-monitor replay risk
  and the signet stall early. But it front-loads the largest and riskiest
  refactor while the node still can't sweep force-closes or set fees — mainnet
  is weeks further out.
- Effort: L (coordinator alone is the design doc's 7-PR plan).

### Approach C: Storage-first (VSS mirror before onchain work)
- Sketch: Abstract `LampoPersistence` behind LDK's `KVStore` trait, add a
  `MirroredStore` (filesystem primary + vss-client secondary with a durability
  policy and replay journal), plus a restore tool; onchain fixes follow.
- Affected files: `lampod/src/persistence/` (new module + mirror store),
  `lampo-common/src/conf.rs`, new `lampo-vss` crate, recovery subcommand in
  `lampod-cli`.
- Tradeoffs: protects against disk loss early and is well-isolated. But today's
  dominant risks are onchain (unswept closes, 1 sat/vB commitments) — a perfect
  backup of a node that can't sweep or fee-bump still loses funds. Wrong order
  for "mainnet soon".
- Effort: M for the mirror, but delays the highest-value fixes.

## Recommendation

Approach A. The user wants mainnet with small funds soon; the unswept-outputs
and hardcoded-fee holes are the ones that actually lose money there, and each
phase-1/2 PR is small and independently shippable. The chain-sync redesign
(already well-documented) lands last with everything else already tested on
signet. Sequence the fee-estimator work so its interface (per-target cache in
`lampo-chain`) survives the later coordinator refactor unchanged.

Suggested phase order (each item ≈ one PR):
1. Real `FeeEstimator` (estimatesmartfee cache, per-target mapping, floors).
2. Broadcast error handling + retry.
3. Hardening batch: early pid lock, accept-policy config, preimage unwrap fix,
   loud corrupt-state warnings.
4. Wire `OutputSweeper` (BDK-backed `ChangeDestinationSource`, sweeper in
   `process_events_async`) + regtest force-close sweep test.
5. Anchors: `UserConfig` negotiation + `BumpTransactionEventHandler` + reserve
   management for anchor spends.
6. Pluggable KVStore + VSS mirror + restore drill.
7. Unified chain sync (per existing design doc).

Mainnet-with-small-funds gate: after item 5. VSS (6) and sync unification (7)
continue while mainnet runs with limited exposure.

## Open questions

- Anchor reserve policy: how many sats to keep unencumbered in the BDK wallet
  for CPFP? (ldk-node uses a per-channel reserve heuristic.)
- Should the VSS mirror block monitor persist on remote ack (stronger, slower)
  or journal-and-replay (weaker, faster)? Leaning journal-and-replay since
  filesystem is primary.
- vss-server deployment/auth model (self-hosted? LNURL-auth? mTLS?) — needed
  before item 6, not before.
- Does the LDK 0.3 git pin expose `OutputSweeper`/`vss-client` compatible
  versions, or does vss-client need a matching git pin?
- Inbound accept policy default for mainnet: closed-by-default vs. allowlist.
