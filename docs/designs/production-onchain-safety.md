# Production On-Chain Safety

Status: Implemented
Date: 2026-08-09
Related: [unified-chain-sync.md](unified-chain-sync.md),
[2026-08-09-lampo-production-fund-safety.md](../brainstorms/2026-08-09-lampo-production-fund-safety.md)

## Motivation

Lampo could not be run on mainnet without risking funds. An audit of the
tree (see the brainstorm document above) found seven concrete fund-loss
vectors, all in the on-chain path:

1. **Channel monitors were never registered on restart.** Monitors were
   read from disk and passed to `ChannelManagerReadArgs`, but no
   `watch_channel` call existed anywhere in the tree. After any restart the
   `ChainMonitor` was empty: counterparty force-closes, revoked
   commitments, and on-chain HTLCs went undetected for every existing
   channel. This was the single worst finding and was not in the original
   audit ranking — it was found while implementing it.
2. **`Event::SpendableOutputs` was dropped.** No `OutputSweeper` was wired
   (`process_events_async` received `sweeper: None`), so funds from any
   closed channel were permanently unswept.
3. **The `FeeEstimator` returned a hardcoded 256 sat/kW** (~1 sat/vB) for
   every `ConfirmationTarget`. Commitment/HTLC/close transactions were all
   built at min-relay fee; under congestion a force-close would not
   confirm before the CLTV deadline. On top of that, the funding path
   interpreted the backend's sat/kvB value as sat/vB, overpaying funding
   fees ~1000x.
4. **No `BumpTransactionEventHandler`** even though anchor channels are
   negotiated by default under LDK 0.3, whose commitments are *designed*
   to need CPFP to confirm.
5. **Broadcast failures were silently swallowed** (fire-and-forget
   `tokio::spawn` discarding the result).
6. **Chain listeners were synced from the ChannelManager's best block**,
   not each listener's own, so a monitor persisted behind the manager
   missed the blocks in between.
7. **Startup races and panics**: the pid lock was taken *after* channel
   state was loaded; the peer manager started concurrently with the
   initial chain sync; `PaymentClaimable` unwrapped an optional preimage.

## What was implemented

One PR, one commit per concern, in dependency order:

| Commit | Concern |
|---|---|
| `chain: Estimate fees per target instead of hardcoding` | Per-target fee cache refreshed from `estimatesmartfee`/`getmempoolinfo`; unified all feerates on sat-per-1000-weight; headroom on `MaximumFeeEstimate` |
| `chain: Surface and retry failed transaction broadcasts` | `Backend::brodcast_tx` returns `Result`; multi-transaction packages relayed atomically via `submitpackage`; "already known" treated as success; bounded retry; `BroadcastFailed` event unblocks waiting RPC callers |
| `node: Register channel monitors on restart` | Per-monitor catch-up sync + `watch_channel`; `sync_chain` runs to completion before peers/events start |
| `node: Sweep spendable outputs of closed channels` | `OutputSweeper` wired end to end (persisted state, BDK change addresses, chain listener, background-processor timer); `SpendableOutputs` replayed on failure |
| `node: Handle BumpTransaction events for anchors` | `BumpTransactionEventHandler` + `WalletSource` primitives on the BDK wallet; anchors made explicit in config; inbound anchor channels require a confirmed on-chain reserve |
| `node: Harden startup and event handling paths` | Early pid lock, `accept-inbound-channels` option, no preimage unwrap, loud corrupt-state warnings, RPC surface gated until the initial sync completes |

### Key design decisions

- **Reuse LDK components, keep Lampo's architecture.** The sweeper is
  `lightning::util::sweep::OutputSweeper`, fee bumping is
  `BumpTransactionEventHandler`, coin selection is LDK's `Wallet` wrapper.
  Lampo contributes only thin adapters: `LampoChangeDestination` and
  `LampoWalletSource` in `lampo-common::wallet`, bridging the
  `WalletManager` trait to LDK's traits. This mirrors ldk-node's choices
  while keeping the `Backend`/facade structure intact.
- **Only `SpendableOutputs` failures are replayed.** Dropping that event
  on a transient persistence failure is fund loss, and replay is the
  LDK-intended mechanism for it. Other events are *not* replayed: LDK
  keeps a failed event at the head of the queue, so replaying an event
  that fails permanently (a funding flow whose peer disconnected, a
  wallet without funds) would block every later event — including
  `PaymentClaimable` — forever.
- **Packages are broadcast atomically.** When LDK hands the broadcaster
  more than one transaction they form a child-pays-for-parent package
  (anchor CPFP + commitment); they are submitted through bitcoind's
  `submitpackage` so the low-feerate commitment cannot be rejected for
  paying below the mempool minimum. "Already known" responses count as
  success, so LDK's rebroadcast timer does not spam error logs.
- **Inbound anchor channels require a reserve.** An anchor channel with
  an empty on-chain wallet cannot CPFP its own force-close; inbound
  anchor channel requests are rejected unless the confirmed wallet
  balance covers a 25k-sat reserve (matching ldk-node's default).
- **Static outputs are not excluded from sweeping.** They pay to scripts
  derived from the LDK keys manager, which the separate BDK wallet does
  not track. Only the sweeper can claim them. (ldk-node excludes them
  because its on-chain wallet shares descriptors with LDK; Lampo's does
  not.)
- **Startup is now strictly ordered**: pid lock → load state →
  `sync_chain` (per-listener catch-up + `watch_channel`) → SPV loop →
  peer manager → background processor → RPC surface. A node that cannot
  complete the initial sync exits instead of going live blind, and the
  httpd does not accept commands while monitors are still unregistered
  (a monitor update issued in that window would be silently dropped).

## Justification for extending the unified-chain-sync design

The existing [unified-chain-sync design](unified-chain-sync.md) plans a
`ChainSyncCoordinator` that folds the BDK wallet and all LDK listeners
into a single `synchronize_listeners` pipeline across ~7 PRs. This work
does **not** implement that coordinator. It does, however, implement two
items from that design's comparison table ahead of schedule, inside the
current architecture:

- per-listener block locators in the initial sync (each channel monitor
  catches up from its own best block), and
- the monitor-registration step (`watch_channel`) the design assumed but
  the code lacked.

The justification for doing these now, out of order:

1. **Correctness cannot wait for architecture.** The missing
   `watch_channel` is a total loss of channel enforcement after restart.
   Landing it inside the coordinator refactor would couple the most
   critical fix in the tree to the largest and riskiest refactor planned.
2. **The work is forward-compatible.** The coordinator design already
   requires per-listener locators and monitor registration; the
   `ChainListeners` struct and `sync_chain`/`listen` split introduced here
   are exactly the seams the coordinator will slot into. Nothing here
   needs to be undone — the coordinator PRs shrink.
3. **The dual-pipeline problem is unchanged.** The BDK wallet still runs
   its own `Emitter` scan; the signet stall documented in the design doc
   is still open and still owned by the coordinator work. This PR neither
   fixes nor worsens it.

## Follow-ups (deliberately out of scope)

In the order recommended by the brainstorm document:

1. **VSS mirror for channel state** — pluggable `KVStore` with
   filesystem primary and `vss-client` mirror, plus a restore drill.
2. **Unified chain sync** — the coordinator from the existing design doc.
3. **Anchor reserve maintenance** — inbound anchor channels are now
   gated on a 25k-sat confirmed balance at accept time, but nothing
   stops the wallet from spending that balance later. A real reserve
   (excluded from coin selection, sized per anchor channel) is still
   needed.
4. **Payment persistence** — `PaymentClaimed`/`PaymentSent` are still
   logged only.
5. **Mnemonic encryption** — `wallet.dat` is still plaintext.
6. **Regtest coverage** — an integration test that force-closes a
   channel, restarts the node, and asserts the sweep lands in the BDK
   wallet would lock in the two biggest fixes here.
