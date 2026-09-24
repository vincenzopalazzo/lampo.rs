# BOLT 12 recurrence in lampo

## Clarified Problem Statement

**Goal:** Lampo v1 speaks BOLT 12 recurrence by running a fork of rust-lightning at PR #4882, creating daily/weekly/monthly offers behind a `recurrence` flag, and paying or cancelling a series through the existing `pay` RPC, with the series id chosen and stored by lampo.

**Decisions (user, 2026-09-24):**

- v1 schedule is a flag: `daily`, `weekly`, or `monthly`. No arbitrary `RecurrencePeriod`, no seconds, no explicit basetime, no paywindow, no limit in the public API.
- Do not add a separate pay method. Extend `pay`.
- Lampo chooses the best `RecurrenceId` key. LDK only offers `RecurrenceId::from_entropy_source` (32 random bytes). It does not derive the id from the offer. Lampo therefore mints one id on the first recurring pay and stores the series under `(offer_id, recurrence_id)`.
- Until #4882 is in a release lampo can pin, lampo builds against a fork of rust-lightning. Not a long-term vendor copy inside this repo, and not "wait for crates.io".

**Constraints:**

- Fork is the `lightning` crate only, pointed at lampo's branch of #4882. The other `lightning-*` crates stay on `0.3.0-beta1` unless the fork does not compile against them; then bump those too, still from crates.io, not a second fork.
- The fork branch is rebased onto the same LDK base lampo already pins. No other LDK behavior changes ride along.
- `offer` accepts `recurrence=daily|weekly|monthly` and maps it to `RecurrencePeriod::Days(1)`, `Days(7)`, or `Months(1)`. Default remains a non-recurring offer.
- v1 recurrence on created offers is optional, not compulsory. A plain `pay` of a recurring offer still works for one shot. A recurring `pay` starts or continues a series.
- `pay` grows optional recurrence fields. Omitted means today's `pay_for_offer` path, byte-for-byte in behavior.
- First recurring `pay` of an offer: lampo generates `RecurrenceId` from the node entropy source, counter `0`, no `prev_state`, and persists `(offer_id, recurrence_id)` before returning.
- Later `pay` of the same offer by the same node loads that series. It does not mint a second id. The call passes stored counter, start, and `prev_state` into `pay_for_recurrence`.
- `PaymentSent` is the only writer of `next_state`. Handler path is `lampod/src/actions/handler.rs`.
- Cancel is a flag on the same `pay` RPC (`cancel=true`), calling `cancel_recurrence` with the stored id. Cancel of an unknown series is an error, not a new id.
- Series state survives restart via existing `LampoPersistence` / `KVStoreSync`.
- No in-process scheduler in v1. The caller asks for the next period.
- Phase 1 recover and Phase 2 soak stay green. Recurrence itself needs its own interop test, not those harnesses.

**Non-goals:**

- Reimplementing TLVs, signing, `recurrence_next_state`, or invoice verification in lampo.
- A timer that auto-pays when a period opens.
- Compulsory recurrence, explicit basetime, paywindow, or `recurrence_limit` in the public v1 API.
- Changing async static-invoice offers.
- Publishing the fork as the upstream lampo release pin without a comment that it is temporary.

**Success criteria:**

- `cargo` resolves `lightning` from the lampo fork branch that contains #4882, and `make check` is green against it.
- `offer` with `recurrence=weekly` decodes back as a weekly recurrence; `offer` without the flag is unchanged.
- `pay` of that offer with the recurrence flag set creates one series, pays period 0 through `pay_for_recurrence`, and after `PaymentSent` the stored `prev_state` is non-empty.
- A second `pay` of the same offer uses the same `RecurrenceId` and the stored state. It does not create a second series.
- `pay` with `cancel=true` on that series calls `cancel_recurrence` and marks the series cancelled.
- Restart, then `pay` the next period, still uses the same id.
- A second lampo node (the payee) invoices the recurring request. This is an interop test, not only a builder test.
- `pay` without the new fields still uses `pay_for_offer`.

## Approaches Considered

### Approach A: Extend `offer` and `pay`, fork-pinned lightning
- Sketch: `[patch]` or a git dependency on a lampo fork branch of rust-lightning at #4882. `json_offer` learns `recurrence`. `json_pay` learns `recurrence` plus `cancel`. `OffchainManager` picks `pay_for_offer` vs `pay_for_recurrence` vs `cancel_recurrence`. A small `RecurrenceSeries` store keys `(offer_id, recurrence_id)` and is updated from `PaymentSent`.
- Affected files: `Cargo.toml`, `lampod/src/jsonrpc/offchain.rs`, `lampod/src/ln/offchain_manager.rs`, `lampod/src/actions/handler.rs`, `lampo-common/src/model/invoice.rs`, new `lampod/src/ln/recurrence.rs`.
- Tradeoffs: Matches the three decisions directly. One RPC surface. Fork is an explicit, removable pin. Risk is API drift if #4882 changes before merge, and `[patch]` surprises anyone who vendors lampo.
- Effort: M

### Approach B: Fork in-tree as a path dependency
- Sketch: Vendor the lightning crate under `vendor/lightning` and depend with `path`.
- Affected files: A, plus a full crate copy.
- Tradeoffs: Builds offline. Review and rebase against upstream LDK become a repo-sized diff. Wrong tool for a temporary protocol PR.
- Effort: L

### Approach C: Separate `recurrencepay` RPC
- Sketch: Leave `pay` alone. New method owns the series.
- Affected files: same as A, plus a new RPC name and CLI subcommand.
- Tradeoffs: Safer against accidental series creation. Rejected: user asked to extend `pay`.
- Effort: M

## Recommendation

Approach A. Pin `lightning` to a lampo fork of #4882. Map the flag onto `Days(1)`, `Days(7)`, and `Months(1)`. On first recurring `pay`, mint `RecurrenceId` from entropy and persist `(offer_id, recurrence_id)`; every later `pay` or cancel reuses that record. That is the best id available: LDK's id is not offer-derived, so the stable key has to be the one lampo stored.

## Open questions

- Fork URL and branch name are not chosen yet. Blocker before implementation, not before the plan.
- Should `recurrence=weekly` on `pay` of a non-recurring offer be a hard error? Recommendation: yes.
- Quantity is in the LDK params and unused by v1. Leave it unset.
