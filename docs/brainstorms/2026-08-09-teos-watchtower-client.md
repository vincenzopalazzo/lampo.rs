# TEOS watchtower client for lampo

## Clarified Problem Statement

**Goal:** Make lampo act as a watchtower *client* of an external
[rust-teos](https://github.com/talaia-labs/rust-teos) tower: on every
channel state update, build the signed justice (penalty) transaction,
encrypt it as a TEOS appointment, and deliver it to a configured tower
so a breach is punished even while lampo is offline.

**Constraints:**
- Lampo plays the client role only — no embedded tower daemon.
- Reuse `teos-common` (git dependency; teos crates are not reliably
  published on crates.io) for appointment types, locator/blob crypto,
  and user registration/auth. Lampo owns the client logic itself.
- Tower unavailability must not wedge channel operation — the LDK
  `Persist` path cannot block on the network.
- LDK 0.3 stack. Use the watchtower APIs LDK ships for this purpose:
  `ChannelMonitorUpdate` interception via a `Persist` wrapper,
  `counterparty_commitment_txs_from_update`, and
  `sign_to_local_justice_tx`.
- Follow existing crate layout conventions (small dedicated crate,
  wiring in `lampod`).

**Non-goals:**
- Running or embedding the `teos` tower daemon inside lampo.
- Tower-side features (accepting appointments from others).
- Multi-tower fan-out and tower reputation handling (design for it,
  don't build it in v1).
- Anchor-channel fee-bumping of justice txs beyond what
  `sign_to_local_justice_tx` produces.

**Success criteria:**
- Full breach test on regtest: lampo opens a channel with a CLN node,
  routes payments, the CLN node is rolled back to a stale datadir
  snapshot and broadcasts a revoked commitment while lampo is stopped;
  the teosd tower detects it and its penalty tx confirms, sweeping
  funds.
- Secondary check: every monitor update while a tower is registered
  produces an appointment the tower ACKs (accepted-appointment count
  matches revoked-state count).
- Registration, appointment queue, and retry state survive lampo
  restarts.

## Approaches Considered

### Approach A: Persist-wrapper capture + durable async delivery (recommended)
- Sketch: wrap `LampoPersistence` (`FilesystemStore`) in a
  `WatchtowerPersister` implementing LDK's `Persist`. On
  `update_persisted_channel` it extracts the counterparty commitment
  txs from the update, signs justice txs via
  `sign_to_local_justice_tx`, encodes+encrypts a TEOS appointment
  (`teos-common`), and pushes it to a file-backed outbound queue. A
  background tokio task handles tower registration, delivery, retries
  with backoff, and tower-status tracking (mirroring the state machine
  of rust-teos's CLN `watchtower-plugin`: reachable / temporarily
  unreachable / misbehaving / subscription error).
- Affected files: new crate `lampo-watchtower`;
  `lampod/src/persistence/mod.rs` (wrap the store);
  `lampod/src/ln/channel_manager.rs` (ChainMonitor takes the wrapper);
  `lampo-common/src/conf.rs`-equivalent for `watchtower-url` /
  `watchtower-pubkey` config; `lampo-testing` + `tests/tests` for the
  breach test.
- Tradeoffs: channel ops never block on the tower (+), durable
  outbox means missed updates are bounded by disk not memory (+);
  a crash between monitor persist and queue persist could drop one
  appointment (window is tiny; mitigate by writing the queue entry
  before completing the monitor update) (−).
- Effort: L (the code is M; the breach test harness is the long pole).

### Approach B: Synchronous ACK-gated persistence
- Sketch: same capture point, but `update_persisted_channel` returns
  `InProgress` and the monitor update only completes once the tower
  ACKs the appointment. Strongest guarantee: no revoked state exists
  that the tower doesn't know about.
- Affected files: same as A plus async-completion plumbing through
  `ChainMonitor::channel_monitor_updated`.
- Tradeoffs: airtight coverage (+); tower outage stalls payments and
  eventually force-closes channels (−); much trickier failure modes in
  LDK's in-flight-update accounting (−).
- Effort: L, higher risk.

### Approach C: Event-bus consumer (decoupled from Persist)
- Sketch: publish monitor updates onto lampo's internal event/actions
  bus and let a standalone `lampo-watchtower` subsystem consume them,
  keeping `persistence/` untouched.
- Affected files: `lampod/src/actions/*`, new crate, event
  definitions in `lampo-common`.
- Tradeoffs: cleanest separation (+); but the justice-tx APIs need the
  `ChannelMonitor` + update pair at persist time, so the event must
  carry heavyweight state anyway, and delivery guarantees get weaker
  than an inline wrapper (−).
- Effort: L.

## Recommendation

Approach A. It is the pattern LDK's watchtower APIs were designed
around, keeps the tower off the hot path, and lets the retry state
machine copy a design already proven in rust-teos's own CLN plugin.
Approach B's guarantee is tempting but couples channel liveness to
tower liveness, which is the wrong default for a first iteration.

## Suggested phasing

1. `lampo-watchtower` crate: `teos-common` types, registration +
   `add_appointment` HTTP client, durable outbox, retry/backoff.
2. `WatchtowerPersister` wrapper + justice-tx construction; config
   knobs; wire into `LampoChannel`'s ChainMonitor.
3. Test harness: build/fetch `teosd` in CI, point it at the harness
   bitcoind; appointment-ACK integration test.
4. Breach test: datadir-snapshot rollback of the CLN counterparty,
   revoked broadcast with lampo stopped, assert penalty confirmation.

## Open questions

- How to provision `teosd` in CI (build from git is slow; consider a
  cached binary or a nix package in `flake.nix`).
- Which teos API surface to target: HTTP (simpler, what the CLN plugin
  uses) vs gRPC. Leaning HTTP.
- Where the outbox lives: inside the existing `FilesystemStore`
  namespace vs a separate dir under lampo's datadir.
- Whether `sign_to_local_justice_tx` output needs fee-bump handling on
  anchor channels before TEOS will get it confirmed (check teos
  penalty-tx broadcast policy).
- Pin strategy for the `teos-common` git dependency (rev pin +
  dependabot exclusion?).
