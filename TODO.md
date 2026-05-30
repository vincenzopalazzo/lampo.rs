# BLIP-0056 — PoS payment notifications: status & deferred LDK work

Spec: https://github.com/vincenzopalazzo/blips/blob/blip-0056-bolt12-pos-notifications/blip-0056.md

## Implemented

- `payment_notification` / `notification_ack` / `notification_nack` custom onion
  messages (`lampod/src/ln/pos.rs`), with `OnionMessenger` wiring replacing the
  default `IgnoringMessageHandler`.
- `PosOnionHandler`: verifies `sha256(preimage) == payment_hash`, replies
  `ack`/`nack`, and emits `LightningEvent::PosPaymentNotified` /
  `PosNotificationAck`.
- `sendpaymentnotification` JSON-RPC (merchant send primitive).
- End-to-end lampo-to-lampo integration test
  (`tests/tests/src/lampo_tests.rs::pos_payment_notification_roundtrip`).

## Pending implementation (tracked, not yet wired)

- PoS-built notification `BlindedMessagePath` carrying the order context
  (`MessageContext::Custom`), order store, and amount verification against the
  decrypted context.
- Merchant auto-send from the `PaymentClaimed` event, correlated by `offer_id`
  (`Bolt12OfferContext`), plus per-order offer construction.
- "v1 bridge" RPC: PoS registers `{offer_id -> notification_path}` with the
  merchant over their peer link (stands in for TODO-1/TODO-2 below).
- Lite `pos_mode` boot (no chain backend / no channels / no gossip).

## Deferred — blocked on LDK (lightning 0.1.8)

### TODO-1: `notification_path` as an experimental offer TLV (range 1e9..2e9)

LDK exposes no PUBLIC builder/reader for experimental offer TLVs — only the
test-only `pub(super) fn experimental_foo` (`offers/offer.rs`). The experimental
range IS round-tripped on parse, so the gap is API-only. This is identical in
lightning 0.1.8 and 0.2.2, so bumping does not unlock it.

- Needs: an upstream LDK change exposing a public experimental-TLV setter +
  reader on `OfferBuilder` / `Offer` / `InvoiceRequest`.
- v1 plan: the PoS registers its notification path directly with the merchant,
  keyed by `offer_id`, instead of embedding it in an offer TLV.
- Spec ref: §Per-Order Offer Construction.

### TODO-2: embed `notification_path` in the invoice payment-path `path_id`

`ChannelManager`'s automatic offer -> `invoice_request` -> invoice responder
owns the `path_id`; there is no public hook to inject custom path context for
offer-inbound payments.

- Needs: an upstream LDK hook for custom path context in the offers responder,
  OR a manual reimplementation of the offer-inbound flow in lampo.
- v1 plan: correlate the notification via `offer_id` + the PoS-built
  notification path's `MessageContext`, not `path_id` round-tripping.
- Spec ref: §Seven-Step Payment Flow, steps 3 & 5.
