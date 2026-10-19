# Brainstorm: splicing aligned with ldk-node

**Goal:** Add operator splice-in and splice-out on an existing Lampo channel, using the same LDK calls as ldk-node, so a later Phoenix LSP funding request can reuse that path.

**Inferences** (no interactive round; correct these before `/ship`):

- The first slice is the operator API. PR 636's on-the-fly funding stays a follow-up.
- Stay on the pinned rust-lightning rev `e8c981e71`. Do not bump LDK for this slice.
- A fee bump ships with splice-in/out, because a second splice is refused while one is pending.
- Restart retry of an unsigned splice (ldk-node #930) is out of this slice.
- Proof is two Lampo nodes on regtest. No ACINQ LSP in this slice.

**Constraints:**

- Pinned LDK already exposes the API ldk-node calls: `ChannelManager::splice_channel` returns a `FundingTemplate`; `splice_in` / `splice_out` build a `FundingContribution`; `funding_contributed` starts negotiation; `FundingTransactionReadyForSigning` is signed and returned via `funding_transaction_signed`. See ldk-node `Node::splice_in_inner`, `splice_out`, and `bump_channel_funding_fee` in `src/lib.rs`, and the event arm in `src/event.rs`.
- `LampoChannelSigner` already forwards `new_funding_pubkey` and `sign_splice_shared_input`. The wallet signs only its own inputs, as ldk-node's `sign_owned_inputs` does. LDK signs the shared funding input.
- `lampo-bdk-wallet` builds a funding tx in `create_transaction` and does not implement `CoinSelectionSource`. Splice-in needs that trait (ldk-node `Wallet`) or `FundingTemplate::splice_in_inputs`.
- Feerate matches ldk-node: `ChannelFunding` estimate, caller override wins, max is 1.5× the minimum, and a pending splice uses `min_rbf_feerate` when it is still under that max.
- Splice events today hit the `_` arm in `lampod/src/actions/handler.rs` and are only logged. A splice never gets signed until `FundingTransactionReadyForSigning`, `SpliceNegotiated`, `SpliceNegotiationFailed`, and `DiscardFunding` are handled.
- `splice_channel` fails unless the peer's init has splicing and quiescence. Both Lampo nodes must advertise those bits.
- No new crates. Regtest only for the integration test.

**Non-goals:**

- Phoenix `will_add_htlc` → splice. That needs `request_funding` on `splice_init` and `will_fund` on `splice_ack`. Neither the pin nor current rust-lightning (`515b0ecd1`) has those TLVs. ldk-node does not send them either.
- Dual-funded `open_channel2` for a wallet with no LSP channel.
- `update_add_htlc` TLV 41041 (`funding_fee`). PR 636's skim claim stays inert until a rust-lightning change reads it.
- Init TLV 1339 (`will_fund_rates`), `will_fail_htlc`, and BOLT 12 payment-hash recognition.
- Persisting splice intent across restart and disconnect (ldk-node #930). LDK drops an unsigned splice when the peer disconnects; this slice surfaces `SpliceNegotiationFailed` and leaves retry to the caller.
- Reclassifying splices inside a payment store. ldk-node still calls that experimental.

**Success criteria:**

- `splicein` adds on-chain sats to an open channel; `spliceout` pays an on-chain address from channel balance; `bumpchannelfee` replaces the unconfirmed splice at a higher feerate.
- A second splice while one is pending returns the same class of error ldk-node returns (`ChannelSplicingFailed` / contribution pending), and names the bump method.
- After `ChannelReady` for the new funding outpoint, the channel still sends and receives.
- `DiscardFunding` unlocks wallet inputs when negotiation fails.
- A regtest test runs splice-in and splice-out between two Lampo nodes and checks the funding tx and the new channel balance.
- The Phoenix follow-up below is written down and not implemented.

## Approaches Considered

### Approach A: Operator RPCs, same calls as ldk-node

- Sketch: `splicein`, `spliceout`, and `bumpchannelfee` sit next to `fundchannel`. Each looks up the channel, calls `splice_channel`, builds the contribution (`splice_in` via a `CoinSelectionSource` on the BDK wallet, or `splice_out` with one `TxOut`), then `funding_contributed`. The handler signs `FundingTransactionReadyForSigning` and emits splice events on the existing bus. Locktime uses the same tip clamp as channel open (issue #572).
- Affected files: `lampod/src/ln/channel_manager.rs`, `lampod/src/actions/handler.rs`, `lampod/src/jsonrpc/open_channel.rs` (or a sibling), `lampo-httpd`, `lampo-bdk-wallet/src/lib.rs`, `lampo-common` request/response types, `tests/tests`.
- Tradeoffs: lands on the pinned LDK and matches ldk-node's public behavior. Phoenix cannot call it for LSP-funded splices until the TLV exists. Restart still drops an unsigned splice.
- Effort: M

### Approach B: Phoenix-funded splice first

- Sketch: on an accepted `will_add_htlc`, splice with a zero local contribution and `request_funding` for the payment hash. Requires a rust-lightning fork before any Lampo glue. No operator splice-in/out.
- Affected files: vincenzopalazzo/rust-lightning (splice TLV stream), then `lampo-phoenix` which is not on this branch (it lives on PR 636).
- Tradeoffs: this is the feature PR 636 is missing, and it does not interoperate with ldk-node's splice API because that API has no `request_funding`. The fork is the largest item in the PR 636 comment after dual-funded open.
- Effort: L

### Approach C: Internal splice service with a Phoenix stub

- Sketch: a `SpliceSession` in `lampod` with `splice_in`, `splice_out`, and `bump`, plus an unused `counterparty_funding` argument for later. RPC is a thin wrapper. A Phoenix caller is compiled only as a comment or a `todo` behind the `phoenix` feature.
- Affected files: the Approach A set, plus a new module under `lampod/src/ln/`, and a stub that PR 636 would later fill.
- Tradeoffs: the seam is real, but the extra type and the stub are unused. CLAUDE.md says write for today. The TLV shape is still unknown, so the argument would be guessed.
- Effort: M

## Recommendation

Approach A. The pinned LDK and ldk-node already share `splice_channel` → `FundingTemplate` → `funding_contributed` → `funding_transaction_signed`. That is the splice implementation that can be correct against ldk-node now. Approach B cannot start in Lampo. Approach C guesses a TLV that does not exist yet.

Keep the three operations as methods on `LampoChannelManager`, the same place as `open_channel`, so PR 636 can call them later without a new framework.

## Phoenix LSP, after this slice

Order from the PR 636 testnet note. Item 1 is the only one that sits on this splice work.

1. **LDK, then Lampo.** Put `request_funding` on `splice_init` and `will_fund` on `splice_ack`. Lampo, when policy accepts `will_add_htlc` and a channel with the LSP already exists (≥ 400_000 sat, anchors), calls the same `splice_channel` path with the LSP's funding request and writes the purchase from `will_fund`. `cancel_on_the_fly_funding` aborts that splice via `cancel_funding_contributed`. This is not ldk-node `splice_in`: the LSP contributes the sats, and the client may contribute none.
2. **LDK.** Read `update_add_htlc` TLV 41041 into the skimmed fee. Until then the HTLC dies inside LDK as `FinalIncorrectHTLCAmount`, and PR 636's claim rule never runs.
3. **LDK.** Surface init TLV 1339 (`will_fund_rates`) to the custom message handler. Without the rate card every real proposal stays undecided.
4. **Lampo, smaller.** `add_fee_credit` for a payment too small to buy liquidity needs no splice. `will_fail_htlc` needs an exported onion failure builder. BOLT 12 hashes are minted inside LDK, so offer payments stay "unknown hash".
5. **LDK, onboarding only.** Dual-funded open as the opener, with `request_funding` in `open_channel2`, for a Lampo that has no channel with the LSP yet.

Doable against the real LSP with no LDK change, and not part of splicing: a fresh node with no channel, paid by phoenixd, should log a `will_add_htlc` decision and otherwise stay silent.

## Open questions

- RPC names: CLN-style `splicein` / `spliceout` / `bumpchannelfee`, or the ldk-node names. Not blocking; match `fundchannel`.
- One PR for all three RPCs, or bump as a second commit on the same branch. Prefer one branch, two commits, if the bump diff is large.
- Confirm `UserConfig::default()` already advertises splicing and quiescence. If it does not, channel open must turn those bits on or `splice_channel` returns `ChannelUnavailable`.
