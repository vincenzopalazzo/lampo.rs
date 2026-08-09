# Hold payments (hodl invoices)

A hold payment is an incoming payment for an invoice built on an
*external* payment hash: the preimage is known only by the caller of
the API, not by the node. When the payment arrives lampo cannot settle
it, so the HTLCs are kept pending until the caller either claims the
payment with the preimage or fails it back. This is the primitive that
enables submarine swaps, escrow-style flows and atomic exchanges of a
preimage across two protocols.

## API

- `holdinvoice {payment_hash, amount_msat?, description, expiring_in?, min_final_cltv_expiry_delta?}`
  registers the hash in the hold registry and returns a BOLT11 invoice
  for it.
- `holdclaim {payment_preimage}` settles the held payment.
- `holdfail {payment_hash}` fails the held payment back to the sender.
- `listholds {}` returns the registry, the only external observation
  surface today (lampo-httpd has no event stream). In process, a
  `LightningEvent::PaymentHeld` event is emitted when a payment is
  held.

## Design

The `HoldManager` lives on the `LampoDaemon` like the other managers
and is shared by the LDK event handler and the JSON-RPC layer. The
decision path in the `PaymentClaimable` handler never blocks: it
records the hold, emits the event and returns, because the LDK event
loop processes events sequentially and any await here stalls the whole
node (pings, persistence, other payments).

Decision table for an incoming claimable payment:

| preimage known | hash registered | amount ok | action |
|---|---|---|---|
| yes | — | — | claim as today |
| no | yes | yes | hold, emit `PaymentHeld` |
| no | yes | no (underpaid) | fail back, keep the record open |
| no | no | — | fail back (`IncorrectOrUnknownPaymentDetails`) |

## Persistence

Hold records are persisted in the lampo persistence (`holds/`
namespace of the `FilesystemStore`) *before* the hold decision is
returned. This is mandatory, not an optimization: LDK restores the
claimable HTLC set from the channel manager on restart but does **not**
re-emit `PaymentClaimable`, and lampo consumes every event. Without a
durable record a restart would make a held payment unreachable and it
would silently expire. `claim_funds`/`fail_htlc_backwards` keep working
after a restart against the restored claimable set, so rehydrating the
registry is enough.

If the held state cannot be persisted the payment is failed back
instead of held: an unpersisted hold is worse than a rejected payment.

## Hold window

LDK fails pending HTLCs back automatically at
`claim_deadline = cltv_expiry - 39` blocks, and rejects `claim_funds`
past the deadline. With the default `min_final_cltv_expiry_delta` of
42 the hold window is only ~3 blocks, which is why `holdinvoice`
exposes the delta: callers must size it to the intended hold duration
(e.g. 144 for roughly a day). `holdclaim` checks the deadline against
the current height and refuses to claim past it. The automatic
fail-back at the deadline means an operator that does nothing can
never ride a held HTLC into a force-close initiated by our own peer;
the residual risk is a node that is down across the deadline, where
the upstream peer force-closes.

## Follow-ups

- Configurable caps on concurrent holds / total held msat (griefing
  surface: held HTLCs consume channel slots and inbound liquidity).
- An event-streaming endpoint in lampo-httpd so external processes do
  not have to poll `listholds`.
- Reconcile records whose deadline passed while the node was offline
  (today they are dropped lazily when `holdclaim` refuses them).
