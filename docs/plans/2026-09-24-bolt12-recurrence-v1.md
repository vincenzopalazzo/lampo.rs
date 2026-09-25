# Plan: bolt12-recurrence-v1

**Goal:** Lampo v1 speaks BOLT 12 recurrence via a pinned fork, a `recurrence` flag on `offer`/`pay`, and a lampo-owned series store keyed by offer id.

**Fork:** `https://github.com/vincenzopalazzo/rust-lightning.git`, branch `lampo-bolt12-recurrence` (shaavan PR #4882 @ `69faf94df`). Pinned via `[patch.crates-io]` on the `lightning` crate only. Temporary; remove when upstream releases recurrence.

**Affected files:**
- `Cargo.toml` — add `[patch.crates-io] lightning = { git, branch }` + comment.
- `Cargo.lock` — updated by cargo (commit it).
- `lampo-common/src/model/invoice.rs` — `GenerateOffer.recurrence: Option<String>`; `Bolt12Pay.recurrence: Option<String>`, `Bolt12Pay.cancel_recurrence: Option<bool>`; `Bolt12InvoiceInfo` gains `recurrence: Option<RecurrenceInfo>`; `PayResult` gains `recurrence_id: Option<String>`.
- `lampod/src/ln/recurrence.rs` (new) — `RecurrenceCadence` (Daily/Weekly/Monthly parse + `to_ldk_period`), `RecurrenceSeries` record + `RecurrenceStore` on `LampoPersistence` namespace `recurring_payments`, key = hex offer id. One series per offer per node in v1.
- `lampod/src/ln/mod.rs` — register module.
- `lampod/src/ln/offchain_manager.rs` — `pay_recurring_offer` (mint id once, `pay_for_recurrence`), `cancel_recurring_offer` (`cancel_recurrence`, counter>=1), `pay_offer` unchanged.
- `lampod/src/jsonrpc/offchain.rs` — `json_offer` applies `.recurrence(...)` when flag set (optional recurrence, no paywindow/limit); `json_pay` routes to recurring path when `bolt12.recurrence` or `cancel_recurrence` set; hard error if flag set on non-recurring offer or period mismatches.
- `lampod/src/actions/handler.rs` — `PaymentSent`: if payment id matches a pending series payment, store `next_state` + bump counter. `PaymentFailed`: clear pending, keep last good state.
- `lampod-cli` / `lampo-cli` args — expose `--recurrence` on offer/pay, `--cancel-recurrence` on pay.
- `tests/` — interop: node A creates recurring offer, node B pays twice + cancels; restart persistence test for series store.

**Approach:** Pin fork first and get `cargo check` green before touching RPC. Series store copies the `payer_proof.rs` encode/decode + namespace pattern. All LDK recurrence calls stay in `OffchainManager`. `pay_offer` behavior without flags is untouched.

**Edge cases:**
- Crash between series persist and `pay_for_recurrence` call: series exists with counter 0, no pending → next pay reuses id, counter 0. Safe (idempotent retry).
- Crash after PaymentSent before next_state write: series counter lags; next pay replays previous counter with old prev_state → payee rejects stale state, surfaces as payment failure, operator retries after fix. Documented; no auto-retry in v1.
- Two concurrent recurring pays: second sees pending payment id → error `recurrence payment already in flight`. No counter race.
- Cancel of never-paid series: counter would be 0, LDK rejects → return explicit error before calling LDK.
- Flag/offer mismatch (e.g. pay `weekly` vs daily offer): hard error.
- Compulsory offers from other nodes: v1 pays them only as recurring via flag; plain pay of compulsory offer errors from LDK — surface message.

**Test plan:**
- `cargo fmt` + `cargo check` + `cargo test -p lampod recurrence` (store roundtrip, cadence parse, key uniqueness).
- Interop test: two-node recurring pay period 0 and 1, cancel, decode shows recurrence.
- Restart: series survives (store read after reopen).
- Regression: existing pay/offer tests green; Phase 1 recover harness after merge.

**Conventions to follow:**
- `make fmt` before commit; imperative subject ≤50 chars.
- `unwrap` only with `// SAFETY:`; log with `target`.
- Imports `std` → external → `crate::`.
- No drive-by refactors; async static-invoice path untouched.

**Open questions / risks:**
- Upstream #4882 API drift on rebase → re-pin + re-check.
- `[patch.crates-io]` affects all workspace members; verify `lampo-lnd`/`lampo-testing` still resolve.
- One series per offer is a v1 limitation; document it.

**Estimated size:** M (150–250 LOC + tests).
