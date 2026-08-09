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

No swap has been executed end to end, and it is currently blocked
before the swap even starts: a spark wallet cannot be funded against
locally built operators. `claim_deposit` fails with

```text
bitcoin error: invalid transaction: signed tx input has empty witness
```

raised by `verify_finalized_taproot_signature` in the sdk, which
expects the operator to return the deposit tree transaction carrying a
64 byte schnorr signature after server side FROST aggregation. The
operator returns it without one.

This is a version skew, not a bug here: the sdk is pinned at
`8c6abb1` (2026-08-07) while the operators are built from
buildonspark/spark `eaddc41` (2026-08-08), whose head commit changes
that exact deposit signing handshake. Fixing it means pinning the
operators to a build the sdk pin matches, moving the sdk pin forward
to match the operators, or using Lightspark's hosted regtest with
faucet credentials.

`spark_htlc_is_locked_and_claimed_with_the_preimage` is kept as the
reproduction. Until it passes, no swap has been demonstrated and
nothing here should be trusted with money.

Known limits, on purpose and documented in code:
- Direction B trusted window (above).
- A crash during `payfetched` or `claim_htlc` needs manual review: the
  preimage lives in the node's in-memory map and is not yet queryable
  after restart. Surfaced loudly at reconcile, never guessed.
- No fee policy yet: quotes are pass-through.
