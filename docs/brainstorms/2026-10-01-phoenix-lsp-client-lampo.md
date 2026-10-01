# Phoenix (ACINQ) LSP client in Lampo, Layer A

Brainstorm, 2026-10-01. Target repository: `lampo.rs` on `origin/main`
(a4ab9b6, pins `vincenzopalazzo/rust-lightning` rev `e8c981e71`).
Every claim below was checked against that LDK revision, against
lightning-kmp `v1.13.2-6-gc8a47548`, and against `phoenixd` on disk.
No live connection to the testnet3 LSP was made.

## Clarified Problem Statement

**Goal:** Lampo advertises itself to the configured ACINQ LSP as an
on-the-fly-funding client, speaks the nine Phoenix extension messages,
stores feerates, fee credit, pending `will_add_htlc` and purchases,
exposes them over three RPCs, and decides (but does not act) whether an
incoming funded HTLC may be claimed, all without touching upstream LDK.

**Constraints**
- Base on `lampo.rs` `main`. `lampod/src/ln/contacts.rs` and
  `simulations/phoenix-contacts.sh` named in the task exist only on
  `feat/blip42-contacts` (7 commits ahead, built on a different LDK
  branch). Reproduce the two patterns, do not depend on that branch.
- No new dependencies. A new cargo *feature* on `lampod` is acceptable.
- One commit per logical change, `make fmt`, every log call with a
  target, `unwrap` only with a SAFETY comment.
- Install the handler unconditionally; without `phoenix-lsp` it must
  advertise nothing and drop everything.

**Non-goals**
- Channel open, splice, `request_funding`, swap-in, trampoline, taproot.
- Parsing init TLV 1339 (LDK's `Init` keeps only TLVs 1 and 3; confirmed
  at `lightning/src/ln/msgs.rs` of the pinned rev).
- Replying `will_fail_htlc`.
- Any change to rust-lightning, including the fork, unless the user
  opts into Approach B below.

**Success criteria**
- `cargo test -p lampod` green: wire round-trips for all nine messages,
  liquidity-ads encode/decode and fee math, policy evaluation, the four
  `decide_payment_claim` cases.
- Integration test under `tests/tests`: node A configured with node B as
  LSP shows bits 561, 563, 129 in B's `list_peers()` view of A and not
  in a third node's view; B's test-only `send_raw` of 39409 shows up in
  A's `phoenixlsp-info`; a 35025/35027 round trip completes.
- `make check` green.
- Final report lists the deviations in the last section of this doc.

## Verified facts that change the plan

1. **Step 4 cannot go live without an LDK change.** At the pinned rev,
   `create_recv_pending_htlc_info` (`lightning/src/ln/onion_payment.rs`)
   rejects with `FinalIncorrectHTLCAmount` when
   `onion_amt_msat > amt_msat + counterparty_skimmed_fee_msat`, and
   `counterparty_skimmed_fee_msat` comes only from LDK's own
   `update_add_htlc` TLV 65537. Phoenix puts the funding fee in TLV 41041
   (`HtlcTlv.kt`, `FundingFeeTlv`: u64 amount_msat + 32-byte funding
   txid). LDK drops unknown odd TLVs, so the fee is `None` and the HTLC
   is failed inside LDK before any Lampo code runs, even with
   `accept_underpaying_htlcs = true`. There is no interception hook for
   final-hop receives. The task's own rule applies: stop and report.
   The pure `decide_payment_claim` extension and its tests can still be
   written and are correct once LDK learns the TLV.
2. **Three message types are even**, not odd as the task states:
   41042 `will_fail_htlc`, 41044 `cancel_on_the_fly_funding`, 41046
   `current_fee_credit`. LDK routes any type the custom reader accepts to
   the handler regardless of parity, but disconnects a peer that sends an
   *unknown* even type. So the reader must always parse all nine types,
   even from non-LSP peers, and let the handler drop them.
3. **`PaymentClaimable` does not carry counterparties.**
   `receiving_channel_ids: Vec<(ChannelId, Option<u128>)>`. Resolve each
   id through `list_channels()` to check every part came from the LSP.
4. **The default message router will not reliably pick the LSP as
   introduction node.** `get_peers_for_blinded_path` lists connected
   peers that support onion messages; the LSP qualifies but is not
   forced. Build the path explicitly with `BlindedMessagePath::new`
   (`MessageForwardNode { node_id: lsp, short_channel_id: None }`) and
   pass a tiny `MessageRouter` to `create_offer_builder_using_router`.
   No channel to the LSP is needed for that.
5. **The reconnect loop only redials channel peers** plus entries in
   `<data>/peers.json`. The LSP has no channel, so startup must call the
   existing `connect` path and the loop must also include the configured
   LSP.
6. **Event emission from the handler needs a late binding.**
   `LampoDaemon::init` builds the peer manager before
   `LampoHandler::new`. Follow `channel_manager().set_handler(...)`: give
   `PhoenixLspHandler` a `set_handler(Arc<dyn Handler>)` slot.
7. **lampo-cli needs no change.** It is a generic passthrough
   (`lampo-cli phoenixlsp-info key=value`). "Mirroring" means the
   `post!` registration in `lampo-httpd/src/commands/*.rs` plus
   `.service(...)` in `lampo-httpd/src/lib.rs`, `server.add_rpc` in
   `lampo-c-ffi`, and response models in `lampo-common/src/model/`.
8. **No cargo features exist on `lampod`**, so a test-only `send_raw`
   needs a new `testing` feature enabled from `lampo-testing`, or it is
   always compiled and marked `#[doc(hidden)]`.
9. **Wire formats match lightning-kmp exactly** for all nine messages,
   `funding_rate`, `will_fund_rates`, payment type bits 0/128/129/130,
   the fee formula, and feature bits 128/129, 560/561, 562/563. Not
   re-read from source: the `recommended_feerates` TLV tags 1 and 3
   (confirm when writing the vector). The task omits the
   `payment_details` encoding needed by `liquidity_ads.rs`: bigsize tag
   (0, 128, 129, 130), bigsize length (32 × n), then n 32-byte items.
10. **kmp's receive rule** (`IncomingPaymentHandler.kt` 386-410): type
    128 and 129 require fee paid ≤ purchase total, type 130 requires fee
    paid == 0, matched by funding txid *and* payment hash. Lampo cannot
    see the txid, so matching by payment hash only is a deliberate
    weakening. Subtracting `fee_credit_used_msat` is stricter than kmp
    and fine.
11. **phoenixd policy** (`Phoenixd.kt`): `--auto-liquidity`
    off/2m/5m/10m sat, `--max-mining-fee` 5k..200k sat (replaces
    `--max-absolute-fee`), `--max-fee-credit` off/50k/125k/250k,
    `--max-relative-fee-percent` default **30 %**, and the absolute check
    looks at mining fee only (`considerOnlyMiningFeeForAbsoluteFeeCheck
    = true`). The task's default of 250 bps is 12× tighter than phoenixd;
    keep it but document the difference. Reject reasons to mirror:
    PolicySetToDisabled, OverRelativeFee, OverAbsoluteFee, OverMaxCredit.
12. Advertising bit 129 (zero reserve) is only a promise; LDK's handling
    of a zero counterparty reserve matters when the LSP opens a channel,
    which is a later task.

## Approaches Considered

### Approach A: Spec on main, LDK gap reported (recommended)
- Sketch: implement Steps 1, 2, 3, 5 in full on `main`. For Step 4 set
  `accept_underpaying_htlcs` on LSP channels, extend
  `decide_payment_claim` with the purchase rule and the four tests, and
  state in code comments and the final report that a real Phoenix
  funded HTLC is rejected inside LDK until TLV 41041 is understood.
- Affected files: new `lampod/src/ln/phoenix_lsp/{mod,wire,
  liquidity_ads,handler,purchases,policy}.rs`; `lampod/src/ln/
  peer_manager.rs` (CMH generic, construction, reconnect, startup dial);
  `lampod/src/lib.rs` (set_handler, accessor); `lampod/src/actions/
  handler.rs` (ChannelReady config update, PaymentClaimable
  destructuring, decide_payment_claim); `lampo-common/src/conf.rs`,
  `event/ln.rs`, `model/phoenix_lsp.rs`; new `lampod/src/jsonrpc/
  phoenix_lsp.rs`; `lampo-httpd/src/commands/phoenix_lsp.rs` +
  `lib.rs`; `lampo-c-ffi/src/lib.rs`; `lampo.example.conf`; `docs/
  designs/phoenix-lsp-client.md`; `tests/tests/src/lampo_tests.rs` or a
  new `phoenix_lsp_tests.rs`; `lampod/Cargo.toml` and
  `lampo-testing/Cargo.toml` for the `testing` feature.
- Tradeoffs: honours every constraint in the task; everything is
  testable with two Lampo nodes; the one thing it cannot do is prove
  Step 4 against a real LSP. Nothing is wasted when the LDK gap closes.
- Effort: L (roughly 10 commits).

### Approach B: A plus a minimal fork patch
- Sketch: same as A, plus one commit in `vincenzopalazzo/rust-lightning`
  (which lampo already pins) that reads TLV 41041 on `UpdateAddHTLC` as
  `funding_fee: Option<(u64, Txid)>` and treats its amount as
  `skimmed_fee_msat` when `accept_underpaying_htlcs` is set, then bumps
  the rev in `lampo.rs/Cargo.toml`.
- Affected files: as A, plus `lightning/src/ln/msgs.rs` and
  `lightning/src/ln/onion_payment.rs` in the fork, and the pinned rev.
- Tradeoffs: makes Step 4 live end to end and testable against
  testnet3; breaks the letter of "no changes to rust-lightning" even
  though upstream stays untouched; adds a fork delta to carry.
- Effort: L + S. Needs the user's explicit go-ahead.

### Approach C: Base on `feat/blip42-contacts`
- Sketch: branch from the blip42 branch so `ContactStore` and the
  `intro_node` offer path can be reused literally as the task says.
- Affected files: as A, minus a store template and the custom router.
- Tradeoffs: saves perhaps 150 lines; costs a dependency on an unmerged
  branch that pins a different LDK branch, a later rebase, and a
  larger diff for reviewers. The user said "updated on main".
- Effort: M for the feature, unknown for the eventual merge.

## Recommendation

**Decision (2026-10-01): Approach A, on `main`.**

Approach A, on `main`. The LDK gap is a fact of the pinned revision,
and the task says to stop and report rather than work around it. Every
other step is independent of it and fully verifiable with two Lampo
nodes. Present Approach B as a one-line decision for the user after
the report; it is cheap because the fork already exists.

## Commit plan for /ship (Approach A)

1. `node: add Phoenix LSP wire messages` (wire.rs + vectors)
2. `node: add liquidity ads types and fee math` (liquidity_ads.rs)
3. `node: add Phoenix purchase store` (purchases.rs)
4. `node: add Phoenix LSP custom message handler` (handler.rs, events)
5. `common: add phoenix-lsp config keys` (conf.rs, example conf, docs)
6. `node: install Phoenix handler in peer manager` (generic, dial,
   reconnect, set_handler)
7. `node: add phoenixlsp RPCs` (jsonrpc, httpd, c-ffi, models)
8. `node: accept LSP funding fees in payment claim` (ChannelReady
   config, decide_payment_claim rule, four tests, LDK-gap comment)
9. `node: evaluate liquidity policy on will_add_htlc` (policy.rs,
   info RPC field)
10. `tests: cover Phoenix LSP handshake and messages` (integration,
    `testing` feature, `send_raw`)

## Open questions

- Approach B: patch the fork now, later, or never?
- Should `phoenix-lsp=default` on regtest/signet error out or leave the
  handler idle? Suggest idle with a warning log.
- `send_raw`: cargo feature `testing` on `lampod` (cleaner) or always
  compiled and `#[doc(hidden)]` (simpler)? Suggest the feature.
- Where do config docs live? `main` has no config page; suggest
  `lampo.example.conf` plus a new `docs/designs/phoenix-lsp-client.md`.
- For the 35025/35027 test, node B must accept 35025 from A. Suggest
  configuring B with A as its LSP too, having the handler emit a
  `PhoenixLspDnsAddressRequest` event for inbound 35025, and letting the
  test reply with `send_raw`.

## Deviations to carry into the final report

- Message types 41042, 41044, 41046 are even.
- `accept_underpaying_htlcs` does not admit Phoenix's TLV 41041; LDK
  fails the HTLC with `FinalIncorrectHTLCAmount` (LDK change needed).
- `PaymentClaimable` exposes channel ids, not counterparties.
- `contacts.rs` and `phoenix-contacts.sh` are not on `main`.
- `payment_details` wire encoding was missing from the task.
- phoenixd's relative-fee default is 30 %, not 250 bps, and its absolute
  check is mining-fee only.
- lampo-cli needs no change; registration is in lampo-httpd and
  lampo-c-ffi.
- Nothing was exchanged with the testnet3 LSP during this brainstorm.
