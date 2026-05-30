# BLIP-0056 — BOLT12 Point-of-Sale Payment Notifications

**Date:** 2026-05-30
**Status:** Plan (pre-implementation)
**Spec:** [blip-0056.md](https://github.com/vincenzopalazzo/blips/blob/blip-0056-bolt12-pos-notifications/blip-0056.md) (authored by the repo owner)
**Target LDK:** stock `lightning 0.1.8` (pinned `0.1.5` in `lampo-common/Cargo.toml`) — **no fork**

---

## Goal

Implement the BLIP-0056 **Point-of-Sale (PoS)** role plus the **merchant-side
notification sender** in lampo, running the PoS in a new **lite daemon mode**
that boots onion messaging *without* channels, chain sync, or gossip. Validate
end-to-end with a lampo-to-lampo integration test.

Target full spec fidelity for the **message types and 7-step flow**; the two
wire-format pieces that stock LDK cannot express yet are **deferred to a tracked
`TODO.md`** rather than forked or hacked around.

---

## Decisions (resolved during brainstorm)

| Fork | Decision | Why |
|------|----------|-----|
| "Without a full node" | **Lite mode inside `lampod`** | Reuses lampo's already-working `OnionMessenger`/`DefaultMessageRouter`/offers wiring; fastest to a working PoS. |
| Scope | **PoS receiver + merchant sender** | The PoS can't be exercised without a node that emits `payment_notification`; both are needed for an in-repo E2E test. |
| Validation | **lampo-to-lampo integration test** | Reuses the existing `tests/tests` harness; strongest in-repo proof. |
| Approach | **A — full spec fidelity** | Owner's call: implement the real flow/message types, not a shortcut. |
| LDK strategy | **Stock LDK; document gaps in `TODO.md`** | No fork, no byte-splice. Build what LDK supports; record the rest as upstream TODOs. |

---

## Key findings that shape the plan

1. **Onion messaging is already wired** — `OnionMessenger::new(...)` at
   `lampod/src/ln/peer_manager.rs:94`, with `DefaultMessageRouter` and BOLT12
   offers (`create_offer_builder`, `pay_for_offer`).
2. **Custom onion messages are dropped today** — the handler is hard-wired to
   `IgnoringMessageHandler` at `lampod/src/ln/peer_manager.rs:102-103`. The
   BLIP-0056 `payment_notification` is a *custom* onion message, so lampo
   currently cannot receive it. This is the central change.
3. **No public LDK API for custom offer TLVs** — in `0.1.8` *and* `0.2.2`. The
   experimental offer TLV range `1_000_000_000..2_000_000_000` is reserved and
   round-tripped on parse, but the only builder method is the test-only
   `pub(super) fn experimental_foo` (`offers/offer.rs:487`). Bumping to 0.2 does
   **not** help → **TODO-1**.
4. **No public hook to set the invoice payment-path `path_id`** for
   offer-inbound payments — `ChannelManager`'s automatic offer responder owns
   it → **TODO-2**.
5. **Daemon boots as one unit** — `LampoDaemon::init` (`lampod/src/lib.rs:203-261`)
   wires chain backend → channel manager → peer manager → gossip together. A
   reduced boot path is new.

---

## Architecture (v1)

**Actors (lite mode):**
- **PoS** — lampo in lite mode: `LampoKeysManager` + `OnionMessenger` +
  `PeerManager` (with `ErroringMessageHandler` as the channel handler, i.e. no
  `ChannelManager`), no chain backend, no gossip. Connects to the merchant as
  its onion-message **introduction peer**.
- **Merchant** — a normal full lampo node with channels/liquidity.
- **Customer** — any node that can pay a BOLT12 offer (a second full lampo node
  in the test).

**v1 flow** (faithful to the spec's message types; the offer-TLV/`path_id`
mechanisms of spec steps 3 & 5 are bridged — see TODO-1/2):

```
1. PoS builds a per-order BOLT12 offer via the merchant
   (offer points to the merchant for invoice_request; offer_id known to PoS).
2. PoS builds a notification BlindedMessagePath back to itself, embedding the
   order context (order_id + expected amount_msat) as MessageContext::Custom.
3. [v1 BRIDGE] PoS registers {offer_id -> notification_path} with the merchant
   over their peer link via a new RPC. (Spec: this travels in an offer TLV +
   invoice path_id; deferred to TODO-1/2.)
4. Customer pays the offer -> merchant receives PaymentClaimed with
   PaymentPurpose::Bolt12OfferPayment { payment_context.offer_id, preimage, .. }.
5. Merchant looks up the notification_path by offer_id and sends a
   `payment_notification` { payment_hash, preimage, amount_msat } custom onion
   message on it, with a reply path.
6. PoS receives the message; LDK hands back the decrypted MessageContext
   (order_id + expected amount). PoS verifies amount_msat == expected and
   sha256(preimage) == payment_hash, then replies `notification_ack`
   (or `notification_nack`) on the reply path, and emits a "paid" event.
```

The **v1 bridge** (step 3) uses only data stock LDK exposes (`offer_id` is
present at `PaymentClaimed`) and is the documented stand-in for TODO-1/2.

---

## Build now — supported by stock LDK 0.1.8

| # | Piece | Location | Key LDK API |
|---|-------|----------|-------------|
| 1 | Lite boot mode | `lampo-common/src/conf.rs:10` (flag), `lampod/src/lib.rs:203-261` (reduced `init`) | `ErroringMessageHandler` (`ln::peer_handler`) |
| 2 | BLIP-0056 message codec + `CustomOnionMessageHandler` (replaces `IgnoringMessageHandler`) | `lampod/src/ln/peer_manager.rs:102-103`, new `lampod/src/ln/pos.rs` | `OnionMessageContents`, `CustomOnionMessageHandler`, `Responder` |
| 3 | PoS notification blinded path + order store + amount/preimage verify + ack/nack | new `lampod/src/ln/pos.rs` | `BlindedMessagePath`, `MessageContext::Custom` |
| 4 | v1 bridge: PoS→merchant notification-path registration RPC + per-order offer build | new RPC in `lampod/src/jsonrpc/`, `lampod/src/jsonrpc/offchain.rs:35` | `create_offer_builder`, `OfferId` |
| 5 | Merchant sender on payment, keyed by `offer_id` | `lampod/src/actions/handler.rs:313` (`PaymentClaimed`) | `OnionMessenger::send_onion_message`, `Bolt12OfferContext::offer_id` |
| 6 | New `LightningEvent` (`PosPaymentNotified`) + RPC/HTTP surface | `lampo-common/src/event/ln.rs`, `lampo-httpd/src/lib.rs` | — |
| 7 | lampo-to-lampo integration test | `tests/tests/src/lampo_tests.rs` | existing test harness |
| 8 | `TODO.md` (LDK gaps) + docs note | repo root / `docs/` | — |

**Receiving mechanism note:** when the merchant sends the notification on the
PoS-built blinded path, LDK decrypts the path and hands the PoS its own
`MessageContext` bytes via `CustomOnionMessageHandler::handle_custom_message(msg,
context, responder)`. That is how the order context (order_id + expected amount)
round-trips without `path_id`. The `Responder` carries the merchant's reply path
for `notification_ack`/`notification_nack`.

---

## Implementation phases (one self-contained commit each)

Each commit must pass `make fmt` + `make check` independently (CLAUDE.md).

- **C0 — Spike (de-risk first).** Lite boot mode (#1) + minimal
  `CustomOnionMessageHandler` (#2) that round-trips a **hardcoded**
  `payment_notification` → `notification_ack` between two lite nodes. Goal:
  confirm `ErroringMessageHandler` satisfies the `PeerManager` bound and that
  `DefaultMessageRouter` builds a blinded path for an unannounced, channel-less
  node using the connected peer as introduction node. **Gate the rest on this.**
- **C1 — Lite boot mode.** `pos_mode` config flag + reduced `init` path; connect
  to a configured introduction/merchant peer. Test: node boots in pos mode,
  connects, has no channels/chain/gossip.
- **C2 — BLIP-0056 codec + custom onion handler.** `payment_notification`,
  `notification_ack`, `notification_nack` as `OnionMessageContents`; a handler
  that dispatches by TLV type; replace both `IgnoringMessageHandler`s. Unit
  tests for encode/decode round-trips.
- **C3 — PoS notification path + verification.** Build notification
  `BlindedMessagePath` with `MessageContext::Custom`; order store; verify
  `amount_msat` + `sha256(preimage) == payment_hash`; emit ack/nack +
  `PosPaymentNotified` event.
- **C4 — v1 bridge + offer construction.** New RPC for the PoS to register
  `{offer_id -> notification_path}` with the merchant; per-order offer build
  wiring. (Documents TODO-1/2 as the eventual replacement.)
- **C5 — Merchant sender.** In the `PaymentClaimed` arm
  (`lampod/src/actions/handler.rs:313`, currently only logs), look up the
  notification path by `offer_id` and send `payment_notification`.
- **C6 — RPC/HTTP surface.** Expose PoS order state / "paid" notifications to a
  client (extend `lampo-httpd`).
- **C7 — Integration test.** `tests/tests/src/lampo_tests.rs`: merchant + lite
  PoS + payer; full flow asserts a `notification_ack` and a paid event.
- **C8 — `TODO.md` + docs.** Land the LDK-gaps doc (below) and a short README
  note on pos mode.

---

## Test plan

- **Unit:** codec round-trips (C2); preimage/amount verification incl. the
  `notification_nack` mismatch path (C3).
- **Integration (C7):** boot merchant + lite PoS + payer in `tests/tests`; PoS
  builds offer + registers notification path; payer pays; assert merchant sends
  `payment_notification`, PoS verifies + replies `notification_ack`, and the PoS
  emits `PosPaymentNotified` with the right order_id/amount. Add a negative case
  (tampered amount → `notification_nack`).

---

## `TODO.md` contents (LDK gaps — to be created at implementation time)

```markdown
# BLIP-0056 — deferred items blocked on LDK (lightning 0.1.8)

## TODO-1: `notification_path` as an experimental offer TLV (range 1e9..2e9)
LDK has no PUBLIC builder/reader for experimental offer TLVs — only the
test-only `pub(super) fn experimental_foo` (offers/offer.rs:487). The
experimental range IS round-tripped on parse, so the gap is API-only.
- Needs: upstream LDK PR exposing a public experimental-TLV setter + reader
  on OfferBuilder / Offer / InvoiceRequest.
- v1 workaround in use: PoS registers its notification path directly with the
  merchant over their peer link, keyed by offer_id (see lampod/src/ln/pos.rs).
- Spec ref: §Per-Order Offer Construction.

## TODO-2: embed notification_path in invoice payment-path `path_id`
ChannelManager's automatic offer→invoice_request→invoice responder controls
`path_id`; no public hook to inject custom path context for offer-inbound
payments.
- Needs: upstream LDK PR for a custom path-context hook in the offers
  responder, OR a manual offer-inbound reimplementation in lampo.
- v1 workaround in use: notification correlated via offer_id + the PoS-built
  notification path's MessageContext, not path_id round-tripping.
- Spec ref: §Seven-Step Payment Flow, steps 3 & 5.
```

Suggested location: top-level `TODO.md`, or `docs/blip-0056-ldk-gaps.md` if
scoped naming is preferred.

---

## Risks / open questions

- **`ErroringMessageHandler` as `PeerManager` chan handler** — confirm it
  satisfies the trait bound in 0.1.8 so the PoS carries no `ChannelManager`.
  Fallback: a zero-channel `ChannelManager` with stub chain components.
  (Resolve in C0.)
- **Blinded-path creation for an unannounced, channel-less node** — confirm
  `DefaultMessageRouter` uses the connected merchant peer as the introduction
  node when the network graph is empty. (Resolve in C0.)
- **`offer_id` availability at `PaymentClaimed`** — relies on
  `PaymentPurpose::Bolt12OfferPayment { payment_context: Bolt12OfferContext {
  offer_id, .. } }`. Verify the field is exposed in 0.1.8.
- **`MessageContext` size** — keep order context minimal (order_id + amount);
  the spec warns about onion `path_id` space.

---

## Out of scope (v1)

Optional `payment_proof` (spec step 7), Appendix B pre-payment validation,
CLN/external-wallet interop, cross-restart persistence of orders/payments, and
the two deferred LDK pieces (TODO-1/2) beyond their v1 bridge.
