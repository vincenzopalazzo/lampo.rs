# lampo-spark-swapd

A swap daemon between a [lampo](https://github.com/vincenzopalazzo/lampo.rs)
lightning node (BOLT12 offers) and [Spark](https://github.com/buildonspark/spark),
both embedded in one process.

The design premise: **only a 32 byte payment hash crosses the
lampo↔Spark boundary.** Lampo terminates all of BOLT12 (offers,
invoice requests, blinded paths); Spark only ever sees hash-locked
transfers. Neither side learns the other's protocol.

## How it works

The daemon *is* the node: it constructs the `LampoDaemon` exactly like
`lampod-cli` does and keeps the in-process handler, which gives it
typed RPC calls and the live event stream (no polling). Next to it, a
Breez `spark-wallet` instance holds the Spark leaves.

```
                 ┌──────────────── lampo-spark-swapd ────────────────┐
 LN peers ──────►│ LampoDaemon (LDK)      Engine       SparkWallet   │◄────── Spark operators
                 │   events() ──────────► state ─────► create_htlc   │
                 │   fetchinvoice/pay ◄── machines ◄── claim_htlc    │
                 └───────────────────────────────────────────────────┘
                                        swap api :9736
```

### Direction A — pay a BOLT12 offer from Spark (atomic)

1. `POST /v1/swap/out {offer, amount_msat?}` — the daemon fetches the
   BOLT12 invoice **without paying it** (`fetchinvoice`) and returns a
   quote: the payment hash, the amount, and its Spark address.
2. The caller locks a Spark HTLC to the daemon on that hash.
3. The engine sees the HTLC, pays the invoice (`payfetched`), and the
   settlement reveals the preimage — the only thing that can claim the
   Spark HTLC. Both legs settle or both refund.

Mind the clock: LDK reaps a fetched-but-unpaid invoice after roughly a
minute, so the Spark HTLC must be locked inside the quote window
(default 45s). An expired quote fails cleanly and can be re-requested.

### Direction B — receive on Spark, atomically

1. `POST /v1/swap/in {spark_address, amount_msat, payment_hash}` — the
   caller generates a preimage and sends only its **hash**. The daemon
   issues a **hold invoice** on that hash, so it cannot settle the
   lightning leg on its own.
2. The caller pays the invoice. The payment is *held*, not settled:
   the daemon has delivered nothing and been paid nothing.
3. The engine locks a Spark HTLC to them on the same hash.
4. They claim it with their preimage. That reveal is the only thing
   that lets the daemon settle the held payment.

Neither side can move alone. If they never claim, the Spark HTLC
refunds to the daemon and the held payment goes back to them.

Because the daemon settles *last*, the lightning hold must outlive the
Spark HTLC; the invoice's final CLTV is derived from the Spark expiry
plus `LN_HOLD_MARGIN_SECS`. Direction A is the mirror: the daemon
claims last there, so it refuses to pay unless the counterparty's lock
leaves at least `CLAIM_MARGIN_SECS`.

## State machines

Every transition is persisted (atomic file write) **before** it is
acted on. Illegal transitions are rejected at the type level.

```
A: Quoted ──► LnPaying ──► Claiming ──► Done
B: OfferPublished ──► LnReceived ──► SparkHtlcCreated ──► Done
   (every active state can fail; terminal states are final)
```

## Building and running

The crate is **excluded from the lampo workspace**: `cargo test --all`
at the repo root does not build it, and cargo-deny does not audit its
dependency tree. It carries the Spark stack, roughly 160 crates lampo
itself has no use for, so it is opt-in by construction.

```bash
cargo build --manifest-path lampo-spark-swapd/Cargo.toml
./lampo-spark-swapd/target/debug/lampo-spark-swapd --data-dir ~/.lampo --network regtest
```

Run this **instead of** `lampod-cli`, never alongside it: swapd is the
node, and both take the same `lampod.pid` lock so the second one
refuses to start.

## Configuration

There is no second config file. The swap settings live in the node's
own `lampo.conf`, in the same CLN-style format as every other key:

```text
network=regtest
core-url=http://127.0.0.1:18443
core-user=user
core-pass=pass

# swapd
spark-network=regtest             # defaults to the lampo network
swap-quote-expiry-secs=45         # direction A quote window
swap-htlc-expiry-secs=3600        # lifetime of spark htlcs we create
swap-api-addr=127.0.0.1:9736      # the swap api
# spark-seed-file=/path/to/seed   # default: <data_dir>/<network>/swapd/spark.seed
```

Point it at a self hosted Spark with repeated `spark-operator` lines,
`<id>|<address>|<identity pubkey>[|<ca cert path>]`. Without them the
SDK defaults apply, which are Lightspark's hosted operators:

```text
spark-operator=0|https://localhost:8535|0322ca18fc489ae25418a0e768273c2c61cabb823edfb14feb891e9bec62016510|/tmp/spark-tls/server_0.crt
spark-operator=1|https://localhost:8536|0341727a6c41b168f07eb50865ab8c397a53c7eef628ac1020956b705e43b6cb27|/tmp/spark-tls/server_1.crt
spark-operator=2|https://localhost:8537|0305ab8d485cc752394de4981f8a5ae004f2becfea6f432c9a59d5022d8764f0a6|/tmp/spark-tls/server_2.crt
```

The Spark seed is created on first start next to the swap records.
Both it and lampo's `wallet.dat` are hot wallets — back them up.

## Regtest

Spark's own "regtest" in the Breez SDK points at Lightspark's hosted
operators and needs faucet credentials. For a self contained loop,
run the operator stack from `buildonspark/spark` instead:

```bash
cd spark && docker compose up -d      # postgres, bitcoind regtest, 3 operators
```

Two things that stack does not give you out of the box:

- Its `cert-init` issues self signed certificates, and a rust client
  rejects those with `CaUsedAsEndEntity` because the same certificate
  is both the trust anchor and the leaf. Issue a local CA and sign the
  operator certificates with it, then hand the *CA* to the client.
- Bitcoin's init step is not idempotent: a second `docker compose up`
  after a plain `down` fails with "Database already exists". Use
  `down -v`.

Then `cargo test -- --ignored` runs `tests/spark_regtest.rs` against
it. Those tests are the evidence that the swap design holds: a wallet
reaches the operators, and queries htlcs, with no Spark service
provider anywhere. The daemon is its own provider, so that had to be
true.

## Status

Builds against lampo `feat/bolt12-swap-primitives` (PR #553) and
breez/spark-sdk `8c6abb1`. State machines and the swap store are unit
tested, and `tests/spark_regtest.rs` talks to real local operators.

The spark half of a swap now runs end to end on regtest:
`spark_htlc_is_locked_and_claimed_with_the_preimage` funds a wallet
from the chain, locks a hash-locked htlc, and a second wallet claims
it with the preimage, with the balance landing on the far side. That
is the first execution of any swap machinery against a real Spark
network.

The operator build **must match the sdk pin**. breez/spark-sdk's own
itest Dockerfile pins buildonspark/spark at
`e8ceff40979d27c1c19e31899901b7b47bd591bc`; build the operators from a
different commit and `claim_deposit` fails inside the sdk with
"signed tx input has empty witness", because the deposit signing
handshake has moved. Check out that commit before `docker compose
build`.

Both swap directions now run end to end on regtest, driven by the real
engine `run()` loop:

- `direction_a_full_swap_spark_to_lightning`: a spark user pays a
  bolt12 offer. Two lampo nodes with a channel stand in for the swap
  node and the merchant; the engine quotes the offer, waits for the
  user's spark htlc on the invoice hash, pays the merchant over
  lightning, and claims the htlc with the revealed preimage. Atomic
  for both sides.
- `direction_b_full_swap_lightning_to_spark`: a lightning payer funds
  a spark address atomically. The user keeps the preimage and sends
  only the hash; the test asserts the lightning payment is **still
  held** after the spark htlc is delivered, which is the property the
  whole direction rests on, and that a reused payment hash is refused.

The two lampo nodes run on their own bitcoind and the spark wallets on
the operators' chain: nothing shares a chain, only the payment hash.

Still not production ready, but the protocol, economic and operational
gaps are all closed now:

- **Atomicity.** Both directions are atomic, a payment hash backs
  exactly one swap, the Spark transfer id is chosen and persisted
  before the transfer is sent so a retry cannot deliver twice, the two
  legs' expiries are derived from each other rather than set
  independently, and over- and under-payment are both refused with a
  hard cap.
- **Fee policy.** Quotes charge a spread, `swap-fee-base-sat` plus
  `swap-fee-ppm` (defaults 1 sat + 5000 ppm), so the spark leg is sized
  smaller than the lightning leg by the fee and lightning routing no
  longer comes out of the daemon's pocket. Both directions apply it, and
  Direction A additionally floors the fee at LDK's routing budget
  (1% + 50 sat), so we can never route-pay more than we collected.
- **No held payment is returned while the spark leg is live.** In
  Direction B the daemon only refunds the held lightning payment once
  the paired spark htlc is provably dead — past its expiry *measured
  from when we locked it* plus a safety margin — never at `created_at +
  expiry`, which precedes the lock and would let a counterparty claim
  the still-live spark htlc after being refunded on lightning, draining
  the full payout. Covered by
  `the_held_payment_is_not_returned_before_the_spark_htlc_is_dead`.
- **A stuck lightning payment cannot outlive its spark collateral.** A
  Direction A payment gets a hard CLTV budget (432 blocks) at fetch
  time, and the counterparty's spark htlc must outlive that budget plus
  the claim margin (`min_lock_expiry_secs` in the quote). Without the
  bound, a payment stuck for LDK's default week-long budget could
  settle *after* the htlc refunded — paid out, nothing to claim. For
  the same reason a payment whose outcome is unknown (timeout, crash)
  is never declared failed while the htlc is alive: reconcile waits for
  evidence — the preimage appears (claim) or the htlc dies (close).
- **Every invoice we did not create is amount-validated.** A quote
  refuses an amountless invoice (settleable for any amount, so paying
  one hands over the preimage for a token payment), refuses an invoice
  whose amount differs from what the caller asked for, refuses
  sub-satoshi amounts, and refuses a zero payout on the receive side.
  Above all we never pay a leg until the counterparty's leg is locked
  for what we are about to pay plus the fee. See `AGENTS.md` §7.
- **An error is never read as "nothing happened".** A failed
  `create_htlc` may mean the transfer was created and the response
  lost, so the daemon asks (`transfer_exists`, answerable because the
  transfer id is picked before the call) rather than assuming — an
  assumption that would leave the counterparty holding a live HTLC
  while we thought we still owed one. See `AGENTS.md` §8.
- **Bounded exposure per swap.** `swap-max-sat` (default 0.01 BTC) caps
  both directions: every accepted Direction B swap locks our funds for
  its whole expiry even if the counterparty walks away, so unbounded
  requests would let a costless griefer tie up the treasury. The swap
  list API also returns a sanitized view — stored secrets (the
  Direction A preimage) never leave the daemon.
- **Direction A crash recovery is automatic.** A crash mid-payment
  resolves itself at reconcile: if it died in `LnPaying` the node is
  asked for the preimage over the `paymentpreimage` RPC (settled →
  claim the spark leg, unknown → fail safely); if it died in `Claiming`
  the preimage was persisted before the attempt, so the claim just
  retries. No manual review. Covered by
  `direction_a_recovers_a_crashed_claim`.

Known limits, on purpose and documented in code:
- **Direction B is atomic; the old "trusted window" is gone.** Earlier
  notes listed a window where the caller trusted us between our
  lightning settlement and the spark htlc existing. That belonged to a
  superseded design where we published a BOLT12 offer and generated the
  preimage ourselves. The live flow takes the caller's payment hash and
  issues a hold invoice we *cannot* settle until they claim the spark
  htlc, so the window does not exist. The offer-publishing code has been
  removed to keep the unsafe path from being wired up by accident.
- **A receive swap is refused up front if we cannot fund it.**
  `create_hold_swap` checks the spark balance against the payout before
  issuing the hold invoice, so a caller is not made to pay into a swap
  that would stall. This is necessary, not sufficient: holding enough
  total does not guarantee a spendable denomination (next point), so
  delivery can still fail — safely, with the held payment refunded.
- **Leaf denominations depend on the SSP.** Delivery tries
  `create_htlc` for the exact payout first, which covers whole-leaf and
  matching-denomination cases, and only falls back to `optimize_leaves`
  to reshape. That fallback needs a Spark service provider for the leaf
  swap, so on the operator-only stack an arbitrary partial payout that
  requires minting change from a single leaf can still fail. Whole-leaf
  and matched-amount payouts work today; on-demand denominations need
  the SSP.
