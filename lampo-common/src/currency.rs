//! Fiat-to-millisatoshi conversion for BOLT 12 currency-denominated offers.
//!
//! LDK cannot own an exchange rate: the rate is external, time-dependent, and
//! application-specific. [rust-lightning#3833][] therefore asks the application
//! for an [`ExchangeRateBound`] and keeps the overflow-checked arithmetic
//! inside LDK.
//!
//! The fork lampo builds against
//! (`vincenzopalazzo/rust-lightning`, `lampo/blip42-on-rc3`, v0.3-rc3 plus
//! currency conversion plus BLIP-42 contacts) takes the
//! converter at initiating calls, and keeps a standing
//! `Arc<dyn CurrencyConversion>` table on `ChannelManager` for inbound and
//! asynchronous flows (answering invoice requests, verifying received
//! invoices). [`LampoCurrencyConversion`] implements that crate's
//! `CurrencyConversion` and is passed in both places from the same
//! `currency-rates` config.
//!
//! [rust-lightning#3833]: https://github.com/lightningdevkit/rust-lightning/pull/3833

use std::collections::BTreeMap;

use lightning::offers::currency::{
    CurrencyConversion as LdkCurrencyConversion, ExchangeRate as LdkExchangeRate,
    ExchangeRateBound as LdkExchangeRateBound, Tolerance as LdkTolerance,
};
use lightning::offers::offer::CurrencyCode;

use crate::conf::LampoConf;
use crate::error;

/// One basis point is 0.01%. LDK rejects a lower tolerance at or above 100%.
const MAX_LOWER_TOLERANCE_BPS: u16 = 9_999;

/// Millisatoshi exchange rate for one ISO 4217 minor unit (USD cents, JPY yen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExchangeRate {
    msats_per_minor_unit: u64,
}

impl ExchangeRate {
    pub fn new(msats_per_minor_unit: u64) -> Self {
        Self {
            msats_per_minor_unit,
        }
    }

    pub fn msats_per_minor_unit(self) -> u64 {
        self.msats_per_minor_unit
    }
}

/// Symmetric tolerance around a reference [`ExchangeRate`].
///
/// The shape matches the discussion on rust-lightning#3833: the application
/// names the rate and the slack, and LDK derives the accepted range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tolerance {
    /// Relative slack. `BasisPoints(100)` is ±1%.
    BasisPoints(u16),
}

/// Reference rate plus the slack lampo will accept when paying or receiving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExchangeRateBound {
    rate: ExchangeRate,
    tolerance: Tolerance,
}

impl ExchangeRateBound {
    pub fn new(rate: ExchangeRate, tolerance: Tolerance) -> Result<Self, ()> {
        match tolerance {
            Tolerance::BasisPoints(bps) if bps > MAX_LOWER_TOLERANCE_BPS => return Err(()),
            Tolerance::BasisPoints(_) => {}
        }
        // Reject a bound whose expanded range cannot be represented.
        let _ = rate_range(rate, tolerance)?;
        Ok(Self { rate, tolerance })
    }

    pub fn rate(self) -> ExchangeRate {
        self.rate
    }

    pub fn tolerance(self) -> Tolerance {
        self.tolerance
    }

    /// Inclusive millisatoshi range for `minor_units` of the bound's currency.
    pub fn to_msats_range(self, minor_units: u64) -> Result<(u64, u64), ()> {
        let (min_rate, max_rate) = rate_range(self.rate, self.tolerance)?;
        let minimum = min_rate.checked_mul(minor_units).ok_or(())?;
        let maximum = max_rate.checked_mul(minor_units).ok_or(())?;
        if minimum == 0 || maximum == 0 {
            return Err(());
        }
        Ok((minimum, maximum))
    }
}

fn rate_range(rate: ExchangeRate, tolerance: Tolerance) -> Result<(u64, u64), ()> {
    let Tolerance::BasisPoints(bps) = tolerance;
    if bps > MAX_LOWER_TOLERANCE_BPS {
        return Err(());
    }
    let delta = u128::from(rate.msats_per_minor_unit)
        .checked_mul(u128::from(bps))
        .and_then(|value| value.checked_div(10_000))
        .ok_or(())?;
    let delta = u64::try_from(delta).map_err(|_| ())?;
    let minimum = rate.msats_per_minor_unit.checked_sub(delta).ok_or(())?;
    let maximum = rate.msats_per_minor_unit.checked_add(delta).ok_or(())?;
    if minimum == 0 {
        return Err(());
    }
    Ok((minimum, maximum))
}

/// Fixed rate table loaded from `currency-rates` in `lampo.conf`.
///
/// Rates are operator-supplied. Lampo does not fetch an exchange rate: a node
/// that prices an offer must be able to explain the number it signed.
#[derive(Clone, Debug, Default)]
pub struct LampoCurrencyConversion {
    rates: BTreeMap<String, ExchangeRate>,
    tolerance_bps: u16,
}

impl LampoCurrencyConversion {
    pub fn from_conf(conf: &LampoConf) -> error::Result<Self> {
        let mut rates = BTreeMap::new();
        for (code, msats) in &conf.currency_rates {
            let code = code.to_ascii_uppercase();
            if CurrencyCode::new(code_bytes(&code)?).is_err() {
                error::bail!("invalid currency code `{code}`");
            }
            if *msats == 0 {
                error::bail!("currency rate for `{code}` must be non-zero");
            }
            rates.insert(code, ExchangeRate::new(*msats));
        }
        if conf.currency_tolerance_bps > MAX_LOWER_TOLERANCE_BPS {
            error::bail!(
                "currency-tolerance-bps `{}` must be below 10000",
                conf.currency_tolerance_bps
            );
        }
        Ok(Self {
            rates,
            tolerance_bps: conf.currency_tolerance_bps,
        })
    }

    pub fn supports(&self, currency: &str) -> bool {
        self.rates.contains_key(&currency.to_ascii_uppercase())
    }

    /// Resolve an ISO 4217 minor-unit amount to the reference millisatoshi
    /// value, and the inclusive range a returned invoice may quote.
    pub fn convert_minor_units(
        &self,
        currency: &str,
        minor_units: u64,
    ) -> error::Result<ConvertedAmount> {
        let code = currency.to_ascii_uppercase();
        if CurrencyCode::new(code_bytes(&code)?).is_err() {
            error::bail!("invalid currency code `{code}`: expected 3 ASCII letters");
        }
        let rate =
            self.rates.get(&code).copied().ok_or_else(|| {
                error::anyhow!("no exchange rate configured for currency `{code}`")
            })?;
        let bound = ExchangeRateBound::new(rate, Tolerance::BasisPoints(self.tolerance_bps))
            .map_err(|_| error::anyhow!("currency tolerance for `{code}` is not representable"))?;
        let (minimum_msats, maximum_msats) = bound.to_msats_range(minor_units).map_err(|_| {
            error::anyhow!("currency amount for `{code}` does not fit in millisatoshis")
        })?;
        let amount_msats = rate
            .msats_per_minor_unit()
            .checked_mul(minor_units)
            .ok_or_else(|| error::anyhow!("currency amount for `{code}` overflows"))?;
        Ok(ConvertedAmount {
            currency: code,
            minor_units,
            amount_msats,
            minimum_msats,
            maximum_msats,
        })
    }
}

impl LdkCurrencyConversion for LampoCurrencyConversion {
    fn conversion_range(&self, currency: CurrencyCode) -> Result<LdkExchangeRateBound, ()> {
        let code = currency.as_str();
        let rate = self.rates.get(code).copied().ok_or(())?;
        LdkExchangeRateBound::new(
            LdkExchangeRate::new(rate.msats_per_minor_unit()),
            LdkTolerance::BasisPoints(self.tolerance_bps),
            LdkTolerance::BasisPoints(self.tolerance_bps),
        )
    }
}

/// A currency amount after lampo has applied its configured rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertedAmount {
    pub currency: String,
    pub minor_units: u64,
    /// Reference amount lampo puts on the invoice request when paying.
    pub amount_msats: u64,
    /// Lowest invoice amount lampo will accept for this offer.
    pub minimum_msats: u64,
    /// Highest invoice amount lampo will accept for this offer.
    pub maximum_msats: u64,
}

impl ConvertedAmount {
    pub fn accepts(&self, invoice_amount_msats: u64) -> bool {
        (self.minimum_msats..=self.maximum_msats).contains(&invoice_amount_msats)
    }
}

fn code_bytes(code: &str) -> error::Result<[u8; 3]> {
    let bytes = code.as_bytes();
    if bytes.len() != 3 || !bytes.iter().all(|byte| byte.is_ascii_uppercase()) {
        error::bail!("invalid currency code `{code}`: expected 3 ASCII letters");
    }
    Ok([bytes[0], bytes[1], bytes[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd_converter() -> LampoCurrencyConversion {
        LampoCurrencyConversion {
            rates: BTreeMap::from([("USD".to_owned(), ExchangeRate::new(1_000))]),
            tolerance_bps: 100,
        }
    }

    #[test]
    fn converts_minor_units_with_symmetric_tolerance() {
        let converted = usd_converter().convert_minor_units("USD", 250).unwrap();
        assert_eq!(converted.amount_msats, 250_000);
        assert_eq!(converted.minimum_msats, 247_500);
        assert_eq!(converted.maximum_msats, 252_500);
        assert!(converted.accepts(250_000));
        assert!(converted.accepts(247_500));
        assert!(!converted.accepts(247_499));
        assert!(!converted.accepts(252_501));
    }

    #[test]
    fn unknown_currency_is_rejected() {
        let err = usd_converter()
            .convert_minor_units("EUR", 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("EUR"), "{err}");
    }

    #[test]
    fn zero_minor_units_are_not_payable() {
        assert!(usd_converter().convert_minor_units("USD", 0).is_err());
    }

    #[test]
    fn lower_tolerance_at_100_percent_is_rejected() {
        let rate = ExchangeRate::new(1_000);
        assert!(ExchangeRateBound::new(rate, Tolerance::BasisPoints(10_000)).is_err());
    }
}
