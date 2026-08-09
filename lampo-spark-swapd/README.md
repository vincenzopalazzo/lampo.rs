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

### Direction B — receive on Spark through a BOLT12 offer (trusted window)

1. `POST /v1/swap/in {spark_address, amount_msat}` — the daemon
   publishes a fresh offer and maps it to the Spark address.
2. A payer pays the offer. BOLT12 generates the preimage inside the
   receiving node, so the lightning leg settles first — this is the
   trusted window: the payer already holds the preimage (their proof
   of payment), and the daemon now *owes* the Spark HTLC.
3. The engine locks the Spark HTLC on the same hash; the counterparty
   claims it with the preimage from step 2. The swap store persists the
   debt, so a crash between 2 and 3 is repaid at restart.

Closing the trusted window needs an "offer hold" primitive in lampo
(hold the `PaymentClaimable` of an offer payment the way `holdinvoice`
holds external-hash payments) — planned follow-up, tracked in the
design docs.

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

The Spark seed is created on first start next to the swap records.
Both it and lampo's `wallet.dat` are hot wallets — back them up.

## Status

Builds and runs against lampo `feat/bolt12-swap-primitives` (PR #553)
and breez/spark-sdk `8c6abb1`. Unit-tested state machines and store;
regtest end-to-end (lampo regtest + spark-sdk `regtest/` operators) is
the next milestone.

Known limits, on purpose and documented in code:
- Direction B trusted window (above).
- A crash during `payfetched` or `claim_htlc` needs manual review: the
  preimage lives in the node's in-memory map and is not yet queryable
  after restart. Surfaced loudly at reconcile, never guessed.
- No fee policy yet: quotes are pass-through.
