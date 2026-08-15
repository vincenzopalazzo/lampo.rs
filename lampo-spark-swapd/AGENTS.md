# Reference notes for agents working on swapd

Background reading distilled from other people's production systems, so
the next person does not re-derive it. **Everything here is a lead, not
an instruction.** Boltz solves a related but different problem, and a
pattern that is safe for on-chain HTLCs is not automatically safe for
Spark statechain transfers.

## How to use this file

Each claim is tagged:

- **[verified]** — read directly in the cited source at the pinned ref.
- **[reported]** — surfaced by a code search and not yet confirmed line
  by line. Re-read the source before relying on it.
- **[inference]** — my reasoning about how it applies to swapd. Argue
  with it.

Before porting anything, ask the three questions at the bottom.

---

## Source: BoltzExchange/boltz-backend

Pinned at `4d131ef` (2026-07-27). Cloned to the session scratchpad; not
vendored here. TypeScript in `lib/`, Rust in `boltzr/`.

Boltz has run submarine swaps in production for years, which makes it
the best available evidence about which failure modes are real rather
than theoretical. Note the shape difference: **their counter-leg is an
on-chain HTLC with a block-height timelock; ours is a Spark transfer
with a wall-clock expiry, settled by operator consensus.** Anything
that leans on block heights, mempool visibility, or unilateral on-chain
exit does not port directly.

`security.md` is **[verified]** only a vulnerability-reporting address.
It is not a statement of security properties, so do not cite it as one.

### 1. The client owns the preimage

**[verified]** `createSwap` takes `preimageHash: Buffer` as a required
argument from the caller (`lib/service/Service.ts:1183`), and
`createReverseSwap` takes `preimageHash?: Buffer`
(`lib/service/Service.ts:1779`). Boltz never generates the secret for a
user-facing swap; it only ever learns it when the user reveals it.

**[inference]** This is the property our Direction B now has. It once
lacked it: an earlier design published a BOLT12 offer, so *our* node
generated the preimage and our lightning leg settled unconditionally,
leaving the user trusting us for the window between settlement and the
Spark HTLC existing. That design is gone. The live flow takes the
caller's payment hash and issues a hold invoice via `holdinvoice`,
which we cannot settle until `holdclaim` with the preimage the caller
reveals by claiming the Spark HTLC — so the window is closed. The
offer-publishing primitives were removed so the unsafe path cannot be
wired up again.

**[verified]** `createReverseSwap` also accepts a BOLT12 invoice
"instead of the preimage hash and invoice amount"
(`lib/service/Service.ts:1781-1782`), i.e. the hash is taken from an
invoice the *user's* node produced. Worth studying as the BOLT12-shaped
version of the same property.

**[verified]** Their BOLT12 support has rough edges: a literal
`// TODO: fix this for bolt12` sits at `lib/service/Service.ts:1464`.
Do not assume their BOLT12 paths are as battle-tested as their BOLT11
ones.

### 2. A payment hash may be used once, globally

**[verified]** `checkSwapWithPreimageExists`
(`lib/service/Service.ts:2597-2615`) queries submarine, reverse **and**
chain swap repositories for the same hash and refuses a new swap if any
of them already used it. The check is across swap *types*, not just
within one.

**[inference]** Directly relevant to two of our open gaps. It is the
guard we lack against a reused BOLT12 offer producing a second
unaccounted payment, and it is the shape of the idempotency key we need
so a retried `create_htlc` cannot deliver twice.

### 3. Overpayment is capped, not merely tolerated

**[verified]** From `CHANGELOG.md`: *"add overpayment check for actual
> expected \* 2"*, *"onchain overpayment protection (#599)"*, and
*"prevent invoice overpayment in hold plugin"*.

**[inference]** Three separate fixes on one theme says overpayment is a
real attack surface, not a rounding nuisance. We hit the benign end of
this already: lightning settled a 50,000,000 msat offer for 50,001,000,
and the engine sized the Spark payout on the received amount, which
would have over-delivered on a funded wallet (fixed in `a9103e3` —
deliver the agreed amount, keep the surplus). Their 2x rule suggests we
should also *reject* absurd overpayment rather than silently pocket it,
because an unexplained 10x payment is more likely a bug or an attack
than a gift.

### 4. Racing yourself into paying twice is the classic bug

**[verified]** From `CHANGELOG.md`: *"race condition sending chain swap
lockup twice"*, plus repeated *"setInvoice race condition"* (#432,
#1117), *"race condition which caused claim transactions not being
detected"*, *"prepay minerfee race condition"*, and the HEAD commit
itself is *"fix: swap update event races"* (#1483).

**[inference]** This is empirical confirmation of our worst crash-window
gap. `deliver_spark_htlc` calls `create_htlc` and only then persists
`SparkHtlcCreated`; a crash in between makes reconcile deliver a second
HTLC on the same hash. `SparkWallet::create_htlc` accepts
`transfer_id: Option<TransferId>` and we pass `None`. Generating that id
deterministically and persisting it *before* the call is the fix, and it
mirrors what Boltz needed.

### 5. Timelocks are computed against the real route, not guessed

**[verified]** `lib/service/TimeoutDeltaProvider.ts` holds per-pair
timeout deltas (`:69`, `:116-128`) and queries actual routes with a
`cltvLimit` to derive the worst-case CLTV
(`:262-313`), taking the maximum CLTV across candidate routes
(`:310-313`).

**[verified]** In the same file (`:268-277`) they look up whether an
invoice's payment hash belongs to one of their *own* reverse swaps
before routing, i.e. they detect being asked to pay themselves.

**[inference]** We currently use a fixed `swap-htlc-expiry-secs` and a
fixed quote window, and never compare them against the lightning leg's
actual CLTV. Our two legs are in *different units* — Spark expiry is
wall-clock seconds, lightning is block height — so the safety
requirement (the leg claimed second must outlive the leg claimed first)
is unverified in our code. This is the gap I would treat as most
security-relevant after the atomicity one.

### 6. Fees are percentage plus base, computed into the quote

**[verified]** `lib/service/Service.ts:1754-1758` computes
`holdInvoiceAmount = (onchainAmount + baseFee) / rate`, then
`holdInvoiceAmount / (1 - feePercent)`, then rounds up with `Math.ceil`,
and derives `percentageFee` from the result.

**[inference]** Note the direction of the division: the fee is applied
so the *user* covers it, and rounding is always in the service's favour.
Our quotes are pass-through, so we pay lightning routing fees out of our
own pocket on every Direction A swap — structurally loss-making, not
merely unpriced.

---

## Questions to ask before porting any of this

1. **Does it depend on an on-chain timelock?** Spark HTLC expiry is
   wall-clock and enforced by operator consensus, with unilateral exit
   as the backstop. Block-height reasoning does not transfer.
2. **Does it depend on being able to see or replace a mempool
   transaction?** We have no equivalent lever on the Spark leg.
3. **Who holds the preimage at each step, and what can each party do
   alone?** Write the sequence out. Every gap we have found so far in
   swapd reduces to getting this wrong.

---

## Source: a private security-findings corpus

A review pass took the bug *classes* from a private working tree of
security findings across Bitcoin and Lightning projects
(`bitcoin-security-council/findings`), including entries against swap
daemons, the Spark stack we depend on, and LDK-based nodes.

**Information hazard — read before adding to this section.** Findings
under `found/` in that tree are *undisclosed* vulnerabilities in other
people's software (`Reported upstream: no`), and this file lives in a
public repository. Record the class and what *we* do about it. Never
copy an unreported finding's exploit chain, commit hash, or `file:line`
here — that is someone else's disclosure to make, not ours.

### 7. Validate the amount on any invoice you did not create

The most repeated theft class in the corpus: a swap client accepts a
counterparty's invoice and checks only the payment hash, or computes
`amount - fee` with no lower bound. Both land in the same place — a
**zero-amount invoice**, which is settleable for any amount, so paying
it reveals the preimage for a token payment while the counterparty
takes the full other leg.

What we do (`quote_spark_to_ln`): refuse an amountless invoice; refuse
one whose amount differs from what the caller asked for (bait and
switch between the quote and the invoice we would pay); refuse
sub-satoshi amounts; cap against `swap-max-sat`. Direction B refuses a
zero payout for the same reason. Tests:
`an_amountless_invoice_is_refused`,
`an_invoice_that_does_not_match_the_asked_amount_is_refused`.

The structural defense matters more than any of those, though: we never
pay a leg before the counterparty's leg is locked for at least what we
are about to pay, plus our fee (`quoted_action`). The published thefts
all work by defeating a *fee cap*; none of them survives a check
against the collateral actually locked.

### 8. A failed call is not proof that nothing happened

Another recurring class: a daemon moves funds, the follow-up call
fails, and the failure is recorded as "nothing happened". A lost
response after a successful write means the counterparty holds
something live while we believe we still owe it — and we may then
refund the other leg on top.

What we do: the Spark transfer id is chosen and persisted *before*
`create_htlc`, so "did it actually happen?" has an answer. On any
`create_htlc` error we ask (`SparkLeg::transfer_exists`) before
retrying or giving up, and treat an existing transfer as delivered. The
same rule drives Direction A recovery: an unknown payment outcome is
never recorded as a failure while the counterparty's HTLC is alive.

### 9. Persist intent before acting on it

A related class: initiating a payment in the node *before* writing the
local record, so a persistence failure plus a retry pays twice.

Checked — we already follow the safe order. `advance_spark_to_ln`
transitions to `LnPaying` and persists before calling `payfetched`, the
payment id is fixed so LDK deduplicates a retry, and the preimage is
persisted before the claim it authorizes. Keep it that way: **persist,
then act** is the rule the store is built around.

### 10. Do not log secrets

Preimages and payment secrets reaching logs is a recurring
low-severity finding. Checked: swapd logs *that* a preimage was
revealed, never its value, and the swaps API returns a sanitized view
rather than raw records. Do not add `{preimage}` to a log line or to an
API response.

## Our open gaps, for cross-reference

Tracked in `README.md` and the code. The protocol, economic and
operational gaps are closed; what remains is the Direction B trusted
window and leaf denomination management, which depends on an SSP.
