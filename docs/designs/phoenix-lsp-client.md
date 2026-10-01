# Phoenix LSP client

Lampo can act as a client of the ACINQ Phoenix LSP: the node that
Phoenix wallets buy inbound liquidity from. This first layer speaks the
protocol and keeps the state later layers need. It does not open,
splice or fund a channel, and it does not answer the LSP's funding
proposals yet.

The LSP speaks the BOLTs plus the draft bLIPs 34 (`recommended_feerates`),
36 (on-the-fly funding), 41 (funding fee credit), the BIP 353 address
request, and liquidity ads from BOLT PR 1153 with the 2024 TLV numbering.
The wire formats follow lightning-kmp 1.13.2, which is what the LSP runs.

## Configuration

| Key | Meaning |
| --- | --- |
| `phoenix-lsp` | `NODE_ID@HOST:PORT` of the LSP, or `default` for ACINQ's node on testnet3 or mainnet. Unset leaves the handler installed but idle. |
| `phoenix-auto-liquidity` | Inbound liquidity to request when a payment does not fit, in sat. Unset rejects every proposal. |
| `phoenix-max-fee-credit` | Fee credit the LSP may keep for payments too small to pay their own fee, in sat. Default 0. |
| `phoenix-max-relative-fee-bps` | Maximum funding fee relative to the amount received, in basis points. Default 250. phoenixd defaults to 30%; Lampo is deliberately tighter. |
| `phoenix-max-mining-fee` | Maximum mining fee of a funding transaction, in sat. Required to accept any proposal. |

The default identities are ACINQ's: testnet3
`03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134@13.248.222.197:9735`
and mainnet
`03864ef025fde8fb587d989186ce6a4a186895ee44a926bfc370e2c366597a3f8f@3.33.236.230:9735`.
`default` on any other network is a configuration error.

## What the node does

- Dials the LSP at startup and redials it every 10 seconds while it is
  gone. The LSP has no channel with a new node, so the ordinary
  channel-peer reconnect loop would never bring it back, and that loop
  only runs when a listener is bound; an LSP client needs no listener.
- Advertises the optional feature bits `on_the_fly_funding` (561),
  `funding_fee_credit` (563) and `zero_reserve_channels` (129) in the
  `init` sent to the LSP, and to no other peer.
- Parses every Phoenix message, from any peer, and drops the ones that do
  not come from the LSP. Parsing them all matters: three of the types
  (41042, 41044, 41046) are even, and LDK disconnects a peer that sends
  an unknown even message.
- Keeps the LSP's init features, the last `recommended_feerates`, the
  fee credit it holds, and every `will_add_htlc` it proposed.
- Decides what the liquidity policy makes of each `will_add_htlc` and
  logs it at `info` under the `phoenix-lsp` target. It does not reply:
  the LSP fails the upstream HTLC after a delay on its own.
- Records liquidity purchases in `phoenix_purchases.json` under the
  network data directory and uses them to accept the funding fee the
  LSP takes from a later HTLC.

## RPCs

- `phoenixlsp-info`: configured, node id and address, connected, the
  LSP's feature bits, the feerates, the fee credit, the pending
  proposals with their decision, and the purchases.
- `phoenixlsp-dnsaddress` (`language`, default `en`): builds this node's
  offer with the LSP as introduction node, sends `dns_address_request`
  and waits up to 30 seconds for the BIP 353 address.
- `phoenixlsp-recordpurchase`: admin entry of a purchase with every
  field explicit. It exists so claiming a funded HTLC can be tested
  before the node can buy liquidity itself.

`lampo-cli` forwards any method, so `lampo-cli phoenixlsp-info` works
without a dedicated command.

## Claiming an HTLC that carries a funding fee

The LSP deducts its fee from the HTLC it relays, so the HTLC delivers
less than the onion promised. Channels with the LSP get
`accept_underpaying_htlcs`, and the claim path accepts the shortfall
only when every HTLC part came over a channel with the LSP, a recorded
purchase lists the payment hash with payment type 128 or 129 (a type
130 purchase allows no shortfall), and the shortfall is at most the
purchase's mining plus service fee minus the fee credit already used.
Anything else is failed back, as before.

### Known gap in LDK

LDK only tolerates a shortfall declared through its own
`update_add_htlc` TLV 65537. Phoenix declares the funding fee in TLV
41041 (an amount and the funding txid), which LDK ignores as an unknown
odd TLV. The HTLC is therefore rejected inside LDK with
`FinalIncorrectHTLCAmount` before any Lampo code runs, even with the
config flag on. The claim rule above is correct and tested, but inert
against the real LSP until LDK learns that TLV. There is no Lampo-side
hook for a final-hop receive, so this needs a change in the pinned
rust-lightning fork.

## Not done here

Opening or splicing a channel with `request_funding`, parsing the init
TLV 1339 that carries the LSP's rate card (LDK drops unknown init TLVs,
so `will_fund_rates` has a codec but nothing feeds it), answering a
`will_add_htlc` with `will_fail_htlc`, swap-in, trampoline and taproot
channels.
