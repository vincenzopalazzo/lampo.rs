# BOLT 12 currency conversion

Status: lampo calls a fork of LDK 0.3-rc2. Upstream #3833 is not merged.

## Upstream

[rust-lightning#3833](https://github.com/lightningdevkit/rust-lightning/pull/3833)
is closed and not merged. Published `lightning` 0.3.0-rc2 still rejects
`Amount::Currency`.

The integration lampo builds against is
[`vincenzopalazzo/rust-lightning` `lampo/bolt12-currency-0.3`](https://github.com/vincenzopalazzo/rust-lightning/tree/lampo/bolt12-currency-0.3),
which is the `v0.3-rc2` tag plus the currency API. It does **not** add a
`CurrencyConversion` type parameter to `ChannelManager` (a rate is not
channel state and is not persisted). Initiating calls take a converter;
inbound and asynchronous paths use a standing
`Arc<dyn CurrencyConversion + Send + Sync>` table instead.

Call sites:

- `create_offer_builder_with_conversion(&conversion)` borrows the converter
  for the builder. `create_offer_builder` still rejects currency amounts.
- `pay_for_offer_with_conversion(..., &conversion)` rejects an explicit
  amount outside `Amount::to_msats_range` before sending an invoice request.
  Omitting the amount lets the payee price the invoice. The returned invoice
  is checked against the same converter.
- `ChannelManager` keeps a **standing** table (no new generic parameter:
  `Arc<dyn CurrencyConversion + Send + Sync>`) for inbound and asynchronous
  paths — answering invoice requests, verifying received invoices. The first
  soak proved this necessary: a call-site-only design leaves the payee with
  `NullCurrencyConversion` and every currency invoice request is rejected
  ("The invoice request was rejected by the recipient").
- `pay_for_offer` is unchanged and uses `NullCurrencyConversion`.

## What lampo does

`LampoCurrencyConversion` implements the fork's `CurrencyConversion`.

- Rates come from `currency-rates=USD=1000,EUR=1100` (millisatoshis per
  minor unit). Lampo does not fetch a rate.
- `currency-tolerance-bps` is symmetric slack (default 100 = 1%). A lower
  tolerance at or above 100% is rejected.
- `offer` with `currency` + `currency_amount` encodes `Amount::Currency`.
  `currency_amount` is minor units (USD cents, JPY yen).
- `pay` passes the caller's amount through. It does not substitute a
  converted millisatoshi amount, which would hide the currency from the
  payee.

Every `lightning*` crate is pinned to the same git revision. Mixing the
fork's `lightning` with the crates.io rc2 crates builds two copies of
`lightning-types` and the traits stop matching.

## Still open

- Async receive offers are still built by the static invoice server. A
  currency amount on that path is a separate change.
- The payer and payee currently share one config table. They should be
  allowed to differ; the fork already permits that because the converter
  is an argument, not a field.
- Upstream `ExchangeRate::from_parts` and `Tolerance::AbsoluteMsats` exist
  on the fork. Lampo only writes whole millisatoshis per minor unit and
  basis points, which is what `lampo.conf` can express.
