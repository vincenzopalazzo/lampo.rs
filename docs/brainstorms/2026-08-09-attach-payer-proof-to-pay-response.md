# Attach a BOLT12 payer proof to the `pay` response

## Clarified Problem Statement

**Goal:** When `pay` settles a BOLT12 offer, return a verifiable payer proof (and the payment
preimage) in the RPC response, so the payer can prove to a third party that they paid that invoice.

**Constraints:**

- Only possible on LDK >= 0.3. The `lightning::offers::payer_proof` module is new: absent at the
  old pinned rev `84605cf`, present at `v0.3.0-beta1` (`9174965`). Unlocked by the crates.io bump
  in PR #554. Implements BOLT PR #1295.
- Proof material reaches lampo only via `Event::PaymentSent.bolt12_invoice`. Lampo currently
  **discards** that event (`lampod/src/actions/handler.rs:381`).
- BOLT11 payments have `bolt12_invoice == None`. The response field must be optional.
- Async payments (`PaidBolt12Invoice::StaticInvoice`) cannot produce a proof — `prove_payer*`
  returns `PayerProofError::IncompatibleInvoice`. Must degrade to `None`, not error the pay.
- Disclosure is baked into the signed proof at build time, so re-issuing with different fields
  requires re-building from the original invoice. Hence the persistence requirement below.
- `CLAUDE.md`: keep it simple, no overdesign; `unwrap` only where provably safe.

**Non-goals:**

- No verification side. Lampo produces proofs; it does not gain a "verify this proof" command.
- No proof for keysend / BOLT11 / static-invoice payments.
- Not changing how offers are paid, only what `pay` returns.
- No garbage collection / retention policy for stored proof material in the first cut.

**Success criteria:**

- Paying a BOLT12 offer returns a `payer_proof` bech32 string that round-trips via
  `PayerProof::from_str` and whose `payment_hash()` matches the payment.
- `PayResult` also carries the preimage, closing the existing
  `// FIXME: missing payment preimage` at `lampo-common/src/model/invoice.rs:171`.
- BOLT11 pay still succeeds with `payer_proof: None`.
- `PayResult.path` is not silently regressed (see the wrinkle below).
- The proof material survives the pay call, so a proof can be re-issued later with a different
  disclosure set without re-paying.
- Integration test covering an offer payment asserting a parseable proof, plus a re-issue that
  discloses strictly more fields than the one returned by `pay`.

## What has to be persisted for re-issue

Only two things, because everything else is derivable:

| Store | Why |
| --- | --- |
| `PaidBolt12Invoice` | The proof is built from it. `Writeable`/`Readable` via `impl_writeable_tlv_based_enum!`. A few hundred bytes to ~1KB. |
| `payment_preimage` | Not derivable. `new_derived` enforces `sha256(preimage) == invoice.payment_hash()`, else `PayerProofError::PreimageMismatch`. 32 bytes. |

Explicitly **not** needed:

- **`Nonce`** — `payer_proof.rs:67` states the payer signing key "is re-derived from the invoice's
  own payer metadata, so no separate `Nonce` needs to be stored alongside it".
  `derive_payer_signing_keys(&self, key: &ExpandedKey, secp_ctx)` takes no nonce. The
  `nonce_hashes` inside the module are unrelated: per-TLV blinding nonces minted fresh by
  `SelectiveDisclosure` at build time. Regenerating them on each re-issue is desirable — it keeps
  two proofs of the same payment unlinkable.
- **`PaymentId`** — recoverable via `Bolt12Invoice::verify_using_metadata(expanded_key, secp_ctx)`,
  which returns it. `prove_payer_derived` takes it only as a cross-check guard. Store it anyway as
  the lookup key.
- **Any key material** — `ExpandedKey` comes from the node seed at runtime via
  `NodeSigner::get_expanded_key()`.

So the record is `payment_id -> (PaidBolt12Invoice, payment_preimage)`.

Storage: `LampoPersistence` is already `lightning-persister`'s `FilesystemStore` (a `KVStore`)
and is `Arc`'d into `LampoDaemon` (`lampod/src/persistence/mod.rs:12`, `lampod/src/lib.rs:109`).
Write under a dedicated namespace keyed by hex `payment_id`. No new dependency, no schema
migration, no sqlite involvement.

Access is two free functions taking the concrete store. That is deliberate: lampo has one backend
today, and an interface designed around a single implementation is how you get the wrong
interface. A `PayerProofStore` trait with a blanket impl over `KVStoreSync` was tried here first
and is a trap — LDK requires `KVStoreSync` of every backend, so the blanket impl claims them all
and coherence (E0119) then forbids any backend from supplying its own native implementation.

The persistence interface is being designed separately, against a real second backend, where it
can be validated rather than guessed.

## Relevant API surface (LDK 0.3.0-beta1)

```rust
// events
Event::PaymentSent {
    payment_id: Option<PaymentId>,
    payment_preimage: PaymentPreimage,
    payment_hash: PaymentHash,
    amount_msat: Option<u64>,
    fee_paid_msat: Option<u64>,
    bolt12_invoice: Option<PaidBolt12Invoice>,   // <-- the proof material
}

// building the proof
PaidBolt12Invoice::prove_payer_derived(
    payment_preimage, &expanded_key, payment_id, &secp_ctx,
) -> Result<PayerProofBuilder<DerivedSigningPubkey>, PayerProofError>

// selective disclosure toggles on the builder
.include_offer_description() .include_offer_issuer()
.include_invoice_amount()    .include_invoice_created_at()
.with_proof_note(String)     .include_type(u64)

.build_and_sign() -> Result<PayerProof, PayerProofError>
```

`PayerProof` implements `Bech32Encode` + `FromStr`, so it serialises as one string.
`ExpandedKey` comes from `NodeSigner::get_expanded_key()`; `LampoKeysManager` wraps `KeysManager`
but its `inner` is `pub(crate)`, so `lampo-common/src/keys.rs` likely needs a small accessor.

Always present in a proof: preimage, payment hash, payer signing pubkey, issuer signing pubkey,
invoice signature, proof signature, merkle root. Everything else is opt-in.

## The design wrinkle: `path`

`PaymentSent` carries no `path`. Hop data exists only on `PaymentPathSuccessful`, which is what
`json_pay` returns on today (`lampod/src/jsonrpc/offchain.rs:123`). Switching the terminal event
wholesale to `PaymentSent` would leave `PayResult.path` empty — a silent regression of a field
that works today.

Options, cheapest first:

1. **Correlate in `json_pay`.** `PaymentSent` emits the receipt (preimage + proof) as a distinct
   `PaymentReceipt` event; `PaymentPathSuccessful` keeps emitting the terminal `PaymentEvent`
   with the path. `json_pay` holds the receipt until the terminal event arrives, then merges.
2. **Return on `PaymentSent`, accept empty path.** Simplest, but a documented behaviour change.
3. **Return on `PaymentSent`, drop `path` from `PayResult`.** Honest but breaks the response schema.

Went with (1). LDK emits `PaymentSent` before `PaymentPathSuccessful`, so the receipt is already
in hand when the terminal event lands. Correlating in `json_pay` rather than in the handler keeps
the state scoped to the one RPC call, needs no daemon-level map, and means the response never
depends on the persister write having succeeded.

## Filtering is mandatory, not optional

`Emitter::emit` broadcasts to every subscriber, so two concurrent `pay` calls both see both
payments' events. Before this change that meant a caller could get the wrong path and hash; with
a receipt attached it would leak another payment's **preimage and payer proof**. Every event
therefore carries a hex `payment_id`, and `json_pay` accepts only its own — `pay_offer` and
`pay_invoice` return the `PaymentId` they used so the caller knows what to match on.

## Approaches Considered

### Approach A: Enrich `LightningEvent::PaymentEvent`, return on `PaymentSent` (recommended)

- Sketch: handle `Event::PaymentSent` in `handler.rs` instead of dropping it. Build the proof
  there via `prove_payer_derived`, add `payment_preimage: Option<String>` and
  `payer_proof: Option<String>` to `LightningEvent::PaymentEvent` and to `PayResult`. `json_pay`
  returns when it sees the enriched event.
- Affected files: `lampod/src/actions/handler.rs` (~381), `lampo-common/src/event/ln.rs` (~33),
  `lampo-common/src/model/invoice.rs` (~167), `lampod/src/jsonrpc/offchain.rs` (~96),
  `lampo-common/src/keys.rs` (expanded-key accessor).
- Tradeoffs: proof construction lives next to the event that owns the data; closes the preimage
  FIXME as a side effect. Costs a signing operation on every BOLT12 pay. Requires the `path`
  correlation above.
- Effort: M

### Approach B: Build the proof in `json_pay`, pass raw material through the bus

- Sketch: `handler.rs` forwards `PaidBolt12Invoice` + preimage on the event untouched;
  `json_pay` does the signing and encoding.
- Affected files: same set, but the LDK proof types leak into `lampo-common`'s event enum, which
  currently exposes only plain strings.
- Tradeoffs: keeps the handler dumb and makes disclosure a per-call decision later. But it puts
  non-`Serialize` LDK types on the event bus, which every subscriber then carries. Given the bus
  already suffers from oversized payloads under load, this is the wrong direction.
- Effort: M, worse shape.

### Approach C: New event only, leave `pay` alone

- Sketch: emit a `LightningEvent::PayerProof` on the bus, no RPC change.
- Affected files: `handler.rs`, `lampo-common/src/event/ln.rs`.
- Tradeoffs: smallest blast radius, no `path` problem. But the `pay` command gains nothing, which
  is the actual request.
- Effort: S, does not meet the goal.

## Recommendation

Approach A. It puts the proof where the data already is, and closes the long-standing missing
preimage FIXME in the same change.

**Disclosure default: minimal — LDK's own `default_included_types` (payer_id, payment_hash,
node_id, features) and nothing optional.** An earlier draft of this doc argued the opposite, on
the grounds that an eager proof the payer could never re-issue should carry the receipt fields.
Persisting the invoice removes that argument entirely: disclosure becomes reversible, and the
safe default for a reversible choice is the one that leaks least. Callers opt into
`offer_description` / `offer_issuer` / `invoice_amount` / `invoice_created_at` at re-issue time.
`proof_note` is never auto-populated — it is a per-dispute attestation supplied by the caller.

## Open questions

- **Re-issue surface.** Persisting the material implies a way to use it. Smallest thing that
  works: a `payerproof` RPC taking `payment_id` plus disclosure booleans and an optional
  `proof_note`. Worth confirming that is wanted now, or whether this cut only persists and the
  RPC lands separately.
- **Retention.** Stored invoices accumulate with no expiry. Fine short-term, but the payer is
  holding proof material for every BOLT12 payment forever — worth a `deletepayerproof` or a
  retention setting before this is load-bearing.
- `path` handling — confirm option (1) is worth the correlation state, or accept an empty `path`.
- Should a failed `prove_payer_derived` (key derivation mismatch, static invoice) log-and-continue
  with `payer_proof: None`, or fail the pay? Recommend log-and-continue: the payment already
  succeeded and failing the RPC would misreport it.
- `json_pay`'s unbounded `events.recv()` loop (FIXME at `offchain.rs:114`) is a known hang source
  under load. Adding a second event dependency to that loop makes it slightly more fragile; worth
  bounding it in the same PR or immediately after.
- `0.3.0-beta1` is a pre-release and BOLT PR #1295 is unmerged, so the proof wire format may still
  change. Acceptable for now, but the field should not be treated as a stable API yet.
