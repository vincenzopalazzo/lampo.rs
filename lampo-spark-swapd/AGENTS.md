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

**[inference]** This is the property our Direction B lacks. We publish
a BOLT12 offer, so *our* node generates the preimage, so our lightning
leg settles unconditionally and the user is trusting us for the window
between settlement and the Spark HTLC existing. Adopting "the client
supplies the hash" is what makes the direction atomic, and lampo now
has the primitive for it: `holdinvoice` takes an external payment hash,
and `holdclaim` settles it once we learn the preimage.

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

## Our open gaps, for cross-reference

Tracked in `README.md` and the code. In rough priority order: reusable
BOLT12 offers producing unaccounted payments; the Direction B trust
window; `create_htlc` idempotency across a crash; debt durability when
the process dies between lightning settling and the record being
written; leaf denomination management; and fee policy.
