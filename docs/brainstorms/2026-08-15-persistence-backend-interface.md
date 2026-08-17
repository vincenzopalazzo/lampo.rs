# Persistence backend interface for lampo

## Clarified Problem Statement

**Goal:** Replace the hardcoded `LampoPersistence = FilesystemStore` alias with a
lampo-owned persistence trait (mirroring the `Backend` pattern in
`lampo-common/src/backend.rs`), so lampo can run on production-grade stores —
direct PostgreSQL and LDK's VSS (Versioned Storage Service, itself
Postgres-backed) — while the filesystem store remains the default.

**Decisions made (user-confirmed):**
- The interface is a lampo-owned trait, not bare LDK `KVStoreSync`.
- It carries *all* lampo persistence: LDK state (channel monitors, manager,
  graph, scorer) and lampo-native records (payer proofs, future business data),
  keyed by namespace.
- VSS integration is a first-class goal — specifically for recovering node
  state — and its relationship to the database needed research (answered below).
- The replication / failure-resistance model needed research (answered below).

**Constraints:**
- LDK `0.3.0-beta1`: `KVStoreSync` is the sync, object-safe trait; a blanket
  impl gives `Persist` for anything implementing it. The async `KVStore` exists
  but ChainMonitor's persist path is effectively sync — the trait should be
  `KVStoreSync`-based with async bridging done inside backends (VSS).
- `Arc<FilesystemStore>` is baked into the `LampoChainMonitor` type alias
  (`lampo-common/src/types.rs:29`); the swap to `Arc<dyn …>` must ripple
  through `lampod/src/lib.rs`, `channel_manager.rs`, `handler.rs`,
  `payer_proof.rs`.
- **Fund-safety invariant:** a channel-monitor write must be durable before it
  is acked. Any backend that acks before fsync/replica-commit can cause
  broadcast of stale state → fund loss on force-close.
- New dependencies (`tokio-postgres`/`sqlx`, `vss-client`) need maintainer
  sign-off per CLAUDE.md.

**Non-goals:**
- No lampo-level dual-write/composite store (rejected: replication belongs to
  the storage layer, see research below).
- No active-active / multi-writer operation. One lampod writes at a time.
- No migration tool between stores in the first iteration (worth a follow-up:
  `lampo-cli` command that copies namespaces store→store).

**Success criteria:**
- `lampod` boots and passes integration tests with each backend selected via
  config (e.g. `persistence=fs|postgres|vss` + DSN/endpoint in lampo.conf).
- `payer_proof.rs` and all of `lampod` compile against `Arc<dyn …>` with no
  concrete store type outside the factory that builds it.
- A Postgres-backed run survives kill −9 + restart with consistent channel
  state (the property the fs store's scary module comment warns about).

## Research answers

### VSS ↔ database: how they fit (user question)

Yes, the integration is natural: **vss-server ships with a PostgreSQL
implementation by default** and is a stateless Rust HTTP service — it scales
horizontally by pointing instances at one Postgres cluster. So VSS *is* the
Postgres integration, packaged as a service: lampo speaks the VSS protocol via
the `vss-client` crate (client-side encryption, key obfuscation, retries,
LNURL-auth), and durability/replication live behind vss-server in Postgres.
LDK Node already ships a `VssStore` implementing LDK's KVStore against it —
a proven reference for our impl. VSS "Phase I" (recovery + single-device) is
production-ready; multi-device is still under evaluation — which matches the
stated recovery use-case exactly.

Two distinct deployment shapes fall out:
- **Self-hosted business node:** lampo → Postgres directly (no extra service).
- **Recovery / cloud / fleet:** lampo → vss-server → Postgres cluster.

### Replication & failure resistance (user question: "IDK, need research")

For Lightning specifically, the distributed-systems answer is unusually
conservative, because *stale state is worse than no state*:

- **Single writer, always.** Two lampods on the same channel DB is split-brain
  with fund loss. Failover means the old node is fenced (dead) before the new
  one starts. VSS's per-key versioning (optimistic concurrency) helps fence
  stale writers at the protocol level.
- **Replication belongs to Postgres, not lampo.** The industry pattern (eclair
  ran this way in production; CLN supports a postgres wallet) is synchronous
  streaming replication: `synchronous_commit = on`/`remote_apply` with a
  standby, or a managed HA Postgres (Patroni, RDS multi-AZ, CloudNativePG).
  Lampo's only obligation is: don't ack a write until `COMMIT` returns.
- **Corollary:** the rejected "lampo-level dual-write" idea re-implements a
  database's replication protocol badly; dropping it keeps lampo simple
  (CLAUDE.md: write for today).

## Approaches Considered

### Approach A: Trait first, direct-Postgres backend second, VSS third (phased)
- Sketch: PR 1 — `trait LampoPersistenceBackend: KVStoreSync + Send + Sync`
  in `lampo-common` (with `kind()`, optional health/lifecycle hooks, mirroring
  `Backend`), wrap `FilesystemStore`, switch `types.rs`/`lampod` to
  `Arc<dyn LampoPersistenceBackend>`. PR 2 — `lampo-postgres-store` crate: one
  table `(primary_namespace, secondary_namespace, key, value bytea)`,
  `tokio-postgres`, writes commit before ack. PR 3 — VSS backend on
  `vss-client`, bridging its async API to `KVStoreSync`.
- Affected: `lampo-common/src/{types.rs, lib.rs}` (+ new `persist.rs`),
  `lampod/src/{lib.rs, persistence/mod.rs, ln/*, actions/handler.rs}`,
  new crates `lampo-postgres-store`, `lampo-vss-store`; config parsing in
  `lampod-cli`/`lampo.example.conf`.
- Tradeoffs: fastest path to the self-hosted business deployment; each PR is
  small and independently testable (matches repo PR rules). VSS lands last.
- Effort: M (trait) + M (postgres) + M (vss).

### Approach B: Trait + VSS backend as the one production store
- Sketch: same trait PR, then go straight to `vss-client`; Postgres arrives
  only behind vss-server. No direct-Postgres crate.
- Affected: as above minus `lampo-postgres-store`.
- Tradeoffs: one production backend to maintain, versioning/fencing for free,
  matches the recovery goal directly. But every deployment must run an extra
  HTTP service, and businesses that just want "lampo + my Postgres" can't have
  it; async→sync bridging is on the critical monitor-persist path.
- Effort: M + L.

### Approach C: No lampo trait — adopt `Arc<dyn KVStoreSync>` directly
- Sketch: change aliases to LDK's dyn trait, reuse ecosystem stores
  (ldk-node's SqliteStore, VssStore) as-is.
- Tradeoffs: least code; but no home for lampo concerns (backend kind, health
  checks, future migration hooks) and couples lampo's public surface to LDK's
  trait evolution. User already rejected this direction.
- Effort: S.

## Recommendation

Approach A. The trait PR is small and unblocks everything; direct Postgres is
the shortest route to "production and business oriented" self-hosted nodes and
carries the replication story via standard Postgres HA; VSS then slots in as a
third impl of the same trait for the recovery use-case — and both production
paths end in Postgres anyway. Start the trait as *exactly* `KVStoreSync +
kind()` and let real backends grow the surface (the payer_proof comment's
advice: design against a real second backend).

## Revision (2026-08-15, after ldk-node and Core Lightning review)

User decision: payments belong in a real database, CLN-style — not in KV.

Findings that drove it:
- **ldk-node** routes *everything* (incl. payments) through `KVStore`
  (fs/SQLite/Postgres/VSS) and answers payment queries from a write-through
  in-memory `HashMap` (`DataStore<PaymentDetails>`): fine for wallets, RAM- and
  startup-cost grows with history, no indexes.
- **Core Lightning** keeps one wallet DB (SQLite default, `--wallet=postgres://`)
  with migrations and typed, indexed tables (`payments`, `invoices`,
  `forwards`, `channels`, `channel_htlcs`, `outputs`); `listpays` etc. are
  indexed SQL. Only inherently opaque state is stored as bytes.

Revised interface — hybrid, two parts:
1. **KV part** (`KVStoreSync` supertrait, as planned): LDK-opaque blobs only —
   monitors, manager, graph, scorer, payer proofs. Fund-critical write path
   unchanged.
2. **Typed domain store** (new, CLN-style): `PaymentStore`-like trait with
   `insert_payment` / `update_payment_status` / `get_payment` /
   `list_payments(filter: time range, status, pagination)`. Backends implement
   it natively:
   - Postgres/SQLite: real tables + indexes + schema migrations.
   - Filesystem deployments: ldk-node's in-memory-map-over-KV pattern is an
     acceptable impl at that scale.
   - VSS: not required to implement typed queries; VSS remains the
     **LDK-state recovery** backend. Payment history recovery on business
     nodes = Postgres replication/backups of the same database. (Optional
     later: mirror payment blobs into VSS for recovery only.)

Impact on approaches: Approach A stands, with PR 2 (Postgres) now delivering
both the KV table *and* the first typed `payments` table + migration scaffold.
The domain-store trait can land in PR 1 alongside the KV trait, initially
implemented for fs via the in-memory pattern.

## Revision 2 (2026-08-15): SQLite and Postgres are both required

User decision: support **both** SQLite and Postgres, CLN-style — SQLite as the
zero-ops default database, Postgres for production/HA.

Design consequence: do NOT build two store crates. Build one `lampo-sql-store`
crate implementing both traits (KV table + typed `payments` table + one shared
migration set), selecting the driver by DSN (`sqlite://…` / `postgres://…`) —
the same single-db-layer-with-query-translation approach CLN uses.

Driver question (replaces the earlier tokio-postgres-vs-sqlx open question):
- **rusqlite + tokio-postgres dual driver** (the CLN way). Note rusqlite is
  already in the dependency tree via `lampo-bdk-wallet` → `bdk_wallet`'s
  `rusqlite` feature — likely the easier sell under the dependency policy.
- **sqlx** with `sqlite` + `postgres` features: one async API over both, but a
  heavier new dependency.

Revised phasing:
- PR 1: traits (KV + domain store) in lampo-common; fs impl wraps
  `FilesystemStore` + in-memory payment map; `Arc<dyn …>` swap in types.rs.
- PR 2: `lampo-sql-store` with SQLite driver first (runs in CI with no
  service), schema + migrations shared with Postgres.
- PR 3: Postgres driver in the same crate + integration test behind a
  docker/CI service.
- PR 4: VSS backend (LDK-state recovery), per Revision 1.

## Revision 3 (2026-08-15): crate layout + VSS as shadow deployment

User decisions:

1. **Crate layout** — the stores are separate crates grouped under a
   `lampo-storage/` directory (workspace members), not one dual-driver crate:
   - `lampo-storage/lampo-sqlite`
   - `lampo-storage/lampo-postgres`
   - `lampo-storage/lampo-vss` (shadow layer, see below)
   - suggested: `lampo-storage/lampo-storage-common` for the shared SQL
     schema + migration definitions, so SQLite and Postgres cannot drift.

2. **VSS is a shadow deployment, not a peer backend.** The main persistence
   trait gains `with_vss()`: a provided method that wraps any backend in a
   `VssShadow<B>` decorator (same trait), so every write to the primary
   database is mirrored to VSS and node state is recoverable from VSS
   regardless of which database is primary.

Shadow-write semantics (design constraints for the VSS PR):
- Primary-DB ack is the only ack; the fund-safety invariant is unchanged.
- Mirroring is **asynchronous** with a durable retry queue — VSS being slow
  or down must never stall channel operations.
- A shadow can therefore lag. Recovery-from-VSS is a last-resort path, and
  restoring *stale* channel monitors is the classic fund-loss hazard: the
  shadow must persist a high-water mark / lag marker so recovery tooling can
  show how far behind the copy was, and channel-state recovery must warn
  loudly. (Payments/domain data recovery from a lagging shadow is merely
  lossy, not dangerous.)
- Typed domain records are shadowed as serialized blobs (VSS is KV); they are
  re-imported through the domain store on recovery.

Phasing after Revision 3:
- PR 1: traits (KV + domain store + `with_vss()` hook stub) in lampo-common;
  fs impl; `Arc<dyn …>` swap.
- PR 2: `lampo-storage/lampo-storage-common` + `lampo-storage/lampo-sqlite`.
- PR 3: `lampo-storage/lampo-postgres` + CI service integration test.
- PR 4: `lampo-storage/lampo-vss` shadow decorator + recovery/import tool and
  lag surfacing.

## Revision 4 (2026-08-17): defer VSS

The VSS shadow and `vss-url` configuration were removed from the persistence
interface PR. A write-only, unencrypted shadow is not a complete recovery
backend and exposes sensitive node state to the VSS operator.

VSS will return as a first-class backend together with migration,
restore/import, lag validation, client-side encryption, and key obfuscation.
That work is tracked in [#592](https://github.com/vincenzopalazzo/lampo.rs/issues/592).

## Step-by-step implementation plan

Written to be executed mechanically, one PR at a time. Every PR must build,
pass `make fmt` and `make check`, and be self-contained (CLAUDE.md rule).
Do not start PR N+1 until PR N is merged.

Ground truth call sites as of commit `e496d21` (verify with `grep -rn
"LampoPersistence" lampod lampo-common` before starting):

| File | Lines | What it does |
| --- | --- | --- |
| `lampod/src/persistence/mod.rs` | 12 | `pub type LampoPersistence = FilesystemStore;` |
| `lampo-common/src/types.rs` | 11, 29 | `Arc<FilesystemStore>` inside `LampoChainMonitor` |
| `lampod/src/lib.rs` | 46, 75, 99, 116, 151 | import, `LampoSweeper` alias, struct field, construction, `persister()` |
| `lampod/src/ln/channel_manager.rs` | 43, 49, 67, 76, 111, 160 | import, field, ctor arg, `build_channel_monitor`, `read_channel_monitors` |
| `lampod/src/actions/handler.rs` | 30, 41, 58, 450 | import, field, init from `lampod.persister()`, payer-proof store call |
| `lampod/src/ln/payer_proof.rs` | 112, 127 | `store()` / `load()` take `&Arc<LampoPersistence>` |

---

### PR 1 — persistence traits + `Arc<dyn …>` swap (no behavior change)

**Step 1.1 — verify the two LDK assumptions first.** Both of the following must
hold or the design changes; check before writing code:

```bash
grep -rn "impl.*Persist<.*> for\|trait KVStoreSync" ~/.cargo/registry/src/*/lightning-0.3.0-beta1/src/util/persist.rs | head -20
```

- (a) `KVStoreSync` is object-safe (all methods sync, no generics, no `Self`
  return). If not, keep a concrete enum instead of `dyn` — see fallback 1.6.
- (b) the blanket `impl<…, K: KVStoreSync + ?Sized> Persist<…> for K` carries
  `?Sized`. If it does **not**, `dyn` cannot satisfy `ChainMonitor`'s `P`
  parameter; fallback 1.6 applies.

**Step 1.2 — create `lampo-common/src/persist.rs`.** Start minimal; do not add
speculative methods.

```rust
//! Lampo persistence backend interface.
use std::sync::Arc;

use crate::error;
use crate::ldk::util::persist::KVStoreSync;

/// Persistence backend kind, mirroring `backend::BackendKind`.
pub enum PersistenceKind {
    Filesystem,
    Sqlite,
    Postgres,
}

/// Lampo-owned persistence interface. The `KVStoreSync` supertrait keeps LDK
/// compatibility; lampo-specific concerns hang off this trait.
pub trait LampoPersistenceBackend: KVStoreSync + Send + Sync {
    fn kind(&self) -> PersistenceKind;
}

/// Typed store for lampo domain data that must be *queried*, not just fetched
/// by key. SQL backends implement this with indexed tables; the filesystem
/// backend implements it with an in-memory map seeded from the KV namespace.
pub trait PaymentStore: Send + Sync {
    fn insert_payment(&self, payment: &PaymentRecord) -> error::Result<()>;
    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>>;
    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>>;
}

/// Query filter. All fields `None` means "everything".
#[derive(Default)]
pub struct PaymentFilter {
    pub from_unix_secs: Option<u64>,
    pub to_unix_secs: Option<u64>,
    pub status: Option<PaymentStatus>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}
```

Define `PaymentRecord` / `PaymentStatus` from what `handler.rs` already knows at
`Event::PaymentSent` / `PaymentClaimed` time (payment hash hex, direction,
amount_msat, fee_msat, status, created_at unix secs, optional bolt11/bolt12
string). Keep it flat — it becomes SQL columns in PR 2.

Add `pub mod persist;` to `lampo-common/src/lib.rs`.

**Step 1.3 — filesystem implementation.** Rewrite
`lampod/src/persistence/mod.rs`:

- Keep `FilesystemStore` as the KV engine, wrapped in a struct
  `FsPersistence { inner: FilesystemStore, payments: Mutex<HashMap<String, PaymentRecord>> }`.
- `impl KVStoreSync for FsPersistence` by delegating all four methods to
  `self.inner`.
- `impl LampoPersistenceBackend for FsPersistence { fn kind(&self) -> PersistenceKind { PersistenceKind::Filesystem } }`.
- `impl PaymentStore for FsPersistence` using the ldk-node pattern: mutate the
  map **and** write the single record to KV namespace `payments` on every
  insert/update; `list_payments` filters the in-memory map; seed the map on
  construction by `list`-ing the `payments` namespace.
- Keys in the `payments` namespace MUST be time-prefixed:
  `format!("{created_at:020}-{payment_hash_hex}")`, so range scans and ordered
  reads stay possible on every backend.
- Delete the `LampoPersistence` type alias only after step 1.4 compiles; until
  then keep `pub type LampoPersistence = FsPersistence;` as a bridge.

**Step 1.4 — swap the types.** In this order, compiling after each file:

1. `lampo-common/src/types.rs:29` — `Arc<FilesystemStore>` →
   `Arc<dyn LampoPersistenceBackend>`; drop the now-unused import at line 11.
2. `lampod/src/lib.rs` — line 75 (`LampoSweeper`), 99 (field), 151
   (`persister()` return) → `Arc<dyn LampoPersistenceBackend>`; line 116 stays
   `Arc::new(FsPersistence::new(root_path.into()))`.
3. `lampod/src/ln/channel_manager.rs` — lines 49, 67 → `Arc<dyn …>`. Lines 111
   and 160 pass it through unchanged; if `read_channel_monitors` rejects the
   trait object, that is assumption (b) failing → fallback 1.6.
4. `lampod/src/actions/handler.rs` — line 41 → `Arc<dyn …>`.
5. `lampod/src/ln/payer_proof.rs` — lines 112, 127 → `&Arc<dyn LampoPersistenceBackend>`.
   Delete the module comment paragraph at lines ~106-110 that says the concrete
   store is taken on purpose; it is now obsolete.

**Step 1.5 — `with_vss()` hook stub.** Add to the trait, defaulting to a no-op
so PR 4 can fill it in without touching call sites:

```rust
    /// Wrap this backend so every write is mirrored to a VSS shadow copy.
    /// Default: no shadow configured, returns self unchanged. See PR 4.
    fn with_vss(self: Arc<Self>, _endpoint: Option<&str>) -> Arc<dyn LampoPersistenceBackend>
    where
        Self: Sized + 'static,
    {
        self
    }
```

**Step 1.6 — fallback if `dyn` is impossible.** If step 1.1 shows LDK's blanket
impl is not `?Sized`, do NOT force it: define
`enum LampoPersistence { Fs(FsPersistence), Sql(SqlStore) }`, implement
`KVStoreSync` + `LampoPersistenceBackend` + `PaymentStore` on the enum by
dispatch, and use `Arc<LampoPersistence>` everywhere the plan says `Arc<dyn …>`.
Every later PR is unchanged apart from adding an enum variant.

**Verify PR 1:** `make fmt && make check`, then run the node once and confirm a
restart preserves channel state (no behavior change is the whole point).
Commit message: `persistence: add lampo-owned persistence traits`.

---

### PR 1 — as implemented (notes from the actual change)

Deviations from the plan above, all deliberate:

- **Step 1.1 verification result.** `KVStoreSync` is object-safe and the
  blanket `impl<…, K: KVStoreSync + ?Sized> Persist<…> for K`
  (`lightning-0.3.0-beta1/src/util/persist.rs:727`) does carry `?Sized`, so
  `Arc<dyn LampoPersistenceBackend>` satisfies `ChainMonitor`'s `P`. The enum
  fallback (step 1.6) was not needed.
- **What the plan missed:** `process_events_async` and `OutputSweeper` require
  LDK's *async* `KVStore`, not `KVStoreSync`, and its blanket impl needs
  `Sized` — a trait object cannot satisfy it. The chain-monitor path needed no
  adapter.
- **Do not use LDK's `KVStoreSyncWrapper` for that adapter.** It calls the sync
  method inline and only wraps the finished result in a ready future
  (`lightning-0.3.0-beta1/src/util/persist.rs:220`), so the write and its
  fsync would run on the tokio worker thread that also drives peer I/O. The
  filesystem store's own async impl offloads to `spawn_blocking`
  (`lightning-persister .../fs_store/common.rs:684`), so using the wrapper
  would have been a silent regression. `lampod/src/lib.rs` defines
  `LampoAsyncPersistence` instead, which hands each call to the blocking pool.
  **The SQL backends must keep using it** — a Postgres write is a network
  round-trip, and blocking the runtime on one is worse than blocking on fsync.
- **No wrapper struct for the fs backend.** `LampoPersistenceBackend` is a
  local trait, so it is implemented directly on the foreign `FilesystemStore`
  in `lampo-common/src/persist.rs` — no delegating newtype.
- **`PaymentStore` and `with_vss()` deferred.** Both would have been dead code
  in PR 1 (no caller, no implementation with a database behind it), which
  CLAUDE.md's "write for today" rule rejects. `PaymentStore` lands in PR 2
  alongside the SQL backend that implements it for real; `with_vss()` lands in
  PR 4. The `PersistenceKind` enum ships with only the `Filesystem` variant for
  the same reason, mirroring `BackendKind { Core }`.
- **Construction seam:** `lampod/src/persistence/mod.rs` is now
  `persistence_for(root_path) -> Arc<dyn LampoPersistenceBackend>`, the single
  place PR 2 extends with config-driven backend selection.

### PR 2 — `lampo-storage/lampo-storage-common` + `lampo-storage/lampo-sqlite`

**Step 2.1 — get maintainer sign-off on the driver** (CLAUDE.md dependency
rule) before writing code: `rusqlite` + `tokio-postgres` (CLN-style, and
`rusqlite` is already in the tree via `lampo-bdk-wallet` → `bdk_wallet`'s
`rusqlite` feature) versus `sqlx`. The plan below assumes `rusqlite`.

**Step 2.2 — create the crates.** Add to `Cargo.toml` `members` *and*
`default-members` (both lists, they are separate at lines 2-23):
`"lampo-storage/lampo-storage-common"`, `"lampo-storage/lampo-sqlite"`.

**Step 2.3 — schema in `lampo-storage-common`,** as `&str` constants shared by
both SQL backends so they cannot drift:

```sql
CREATE TABLE IF NOT EXISTS kv (
  primary_namespace   TEXT NOT NULL,
  secondary_namespace TEXT NOT NULL,
  key                 TEXT NOT NULL,
  value               BLOB NOT NULL,
  PRIMARY KEY (primary_namespace, secondary_namespace, key)
);

CREATE TABLE IF NOT EXISTS payments (
  id           TEXT PRIMARY KEY,
  payment_hash TEXT NOT NULL,
  direction    TEXT NOT NULL,
  amount_msat  INTEGER NOT NULL,
  fee_msat     INTEGER,
  status       TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  invoice      TEXT
);
CREATE INDEX IF NOT EXISTS payments_created_at ON payments (created_at);
CREATE INDEX IF NOT EXISTS payments_status_created_at ON payments (status, created_at);
```

Plus a `schema_version` table and a migration runner (`Vec<&str>` of migration
steps applied in order, version row bumped in the same transaction). Postgres
differences (`BYTEA` vs `BLOB`, `BIGINT` vs `INTEGER`) go behind a small
dialect enum in this crate — one schema definition, two renderings.

**Step 2.4 — implement `SqliteStore`** in `lampo-storage/lampo-sqlite`:
`KVStoreSync` over the `kv` table (`write` = upsert, `read` = select returning
`io::ErrorKind::NotFound` when absent — `payer_proof.rs:130` depends on that
exact error kind, `remove` = delete, `list` = prefix select), plus
`LampoPersistenceBackend` and a real SQL `PaymentStore` where `list_payments`
becomes `WHERE created_at BETWEEN ? AND ? AND status = ? ORDER BY created_at
LIMIT ? OFFSET ?`.

Durability: open with `PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;` and
never ack a write before the transaction commits. This is the fund-safety
invariant — do not relax it for benchmarks.

**Step 2.5 — config wiring.** Follow the existing pattern exactly
(`lampo-common/src/conf.rs`: field at ~line 38, default at ~line 76, parse at
~line 272 via `conf.get_conf("…")`):

- add `pub storage: Option<String>` (values `fs` | `sqlite` | `postgres`,
  default `fs`) and `pub storage_url: Option<String>`;
- parse as `storage` / `storage-url`;
- document both in `lampo.example.conf` in the commented style used there;
- build the backend in `lampod/src/lib.rs:116` by matching on `conf.storage`.

**Verify PR 2:** a unit test round-tripping KV values; a `list_payments` test
over ~10k synthetic rows asserting the year-range query returns correctly and
uses the index (`EXPLAIN QUERY PLAN`); then `make check`. Run the full node on
`storage=sqlite` and confirm a channel open survives restart.

---

### PR 3 — `lampo-storage/lampo-postgres`

Same trait impls as PR 2 against `tokio-postgres`, reusing
`lampo-storage-common`'s schema with the Postgres dialect. Specifics:

- Bridge sync `KVStoreSync` calls to the async client on a dedicated runtime
  handle — do **not** call `block_on` on the main runtime thread (it
  deadlocks). A dedicated single-threaded runtime owned by the store, or a
  blocking connection pool, both work; pick one and document it.
- `synchronous_commit` stays at the server default (`on`); note in the crate
  docs that HA is Postgres streaming replication, not lampo's job.
- Add a CI job with a `postgres` service container, gated so the default test
  run does not require it.

**Verify PR 3:** same tests as PR 2 parameterized over both backends, plus a
kill-9-and-restart test asserting channel state consistency.

---

### PR 4 — `lampo-storage/lampo-vss` shadow deployment

- `VssShadow<B>` wraps any `B: LampoPersistenceBackend` and implements the same
  traits: every method delegates to the primary, and writes additionally
  enqueue a mirror job. Fill in `with_vss()` from step 1.5 to return it.
- Mirroring is **async with a durable retry queue** (persist the queue in the
  primary store so it survives restart). The primary's ack is still the only
  ack; VSS being down must never block a channel operation.
- Persist a high-water mark (last successfully mirrored write id / timestamp)
  and expose it via RPC, so an operator can see shadow lag.
- Recovery tooling: a `lampo-cli` command that reads a VSS store and imports it
  into a fresh primary. It MUST print the shadow lag and refuse to restore
  channel monitors without an explicit `--i-understand-stale-state-can-lose-funds`
  style flag. Restoring stale monitors is the known fund-loss hazard.
- Typed payment records are mirrored as serialized blobs (VSS is KV) and
  re-imported through `PaymentStore` on recovery.

**Verify PR 4:** integration test with vss-server in docker — write through the
shadow, wipe the primary, restore, assert channel + payment state matches; and
a test where VSS is unreachable asserting node operations continue unblocked.

---

### Follow-ups (not in these PRs)

- Store-to-store migration command (`lampo-cli migrate-store`).
- `MonitorUpdatingPersister` (incremental monitor updates) — biggest single
  perf lever once the interface is in place.
- Async read-model projection for analytics, only if in-DB queries stop being
  enough.

## Open questions

- Driver choice (`rusqlite` + `tokio-postgres` vs `sqlx`) — needs maintainer
  sign-off per the dependency policy. Blocks PR 2, step 2.1.
- VSS auth story for lampod (LNURL-auth vs fixed headers) — decide in PR 4.
- Should `listen`-style lifecycle (connect/reconnect, health) be on the trait
  from day one, or added when Postgres needs it? Lean: add when needed.
- Whether `PaymentStore` should also absorb invoices and forwards (CLN has
  tables for both) — defer until a concrete RPC needs them.
