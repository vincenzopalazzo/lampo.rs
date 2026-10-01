//! Liquidity ads (BOLT PR 1153, 2024 TLV numbering) as spoken by the ACINQ
//! Phoenix LSP: funding rates, payment types and the fee arithmetic a buyer
//! needs to record a purchase and to check the funding fee taken from a
//! later HTLC against it.
//!
//! All integers are big-endian. The init TLV carrying `will_fund_rates` is
//! not parsed yet, so nothing feeds [`WillFundRates::decode`] at runtime; the
//! codec is here so recorded rates can be used as soon as they are available.

use lampo_common::ldk::io::{self, Read};
use lampo_common::ldk::ln::msgs::DecodeError;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::util::ser::{BigSize, Readable, Writeable, Writer};

/// Fees owed to the seller for one purchase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fees {
    pub mining_fee_sat: u64,
    pub service_fee_sat: u64,
}

impl Fees {
    pub fn total_sat(&self) -> u64 {
        self.mining_fee_sat.saturating_add(self.service_fee_sat)
    }

    pub fn total_msat(&self) -> u64 {
        self.total_sat().saturating_mul(1000)
    }
}

/// One `funding_rate` entry: what the seller charges for amounts in
/// `[min_amount_sat, max_amount_sat]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FundingRate {
    pub min_amount_sat: u32,
    pub max_amount_sat: u32,
    /// Weight the seller adds to the funding transaction for its inputs and
    /// outputs; the buyer pays the mining fee for it.
    pub funding_weight: u16,
    /// Proportional fee on the contributed amount, in basis points.
    pub fee_proportional_bps: u16,
    /// Flat fee paid regardless of the amount.
    pub fee_base_sat: u32,
    /// Flat fee paid only when a new channel is created.
    pub channel_creation_fee_sat: u32,
}

impl FundingRate {
    /// Fees for a purchase of `requested_sat` of which the seller contributes
    /// `contributed_sat`, at `feerate_sat_per_kw`.
    ///
    /// The mining fee is `feerate * funding_weight / 1000`, truncated to a
    /// whole satoshi the way lightning-kmp's `weight2fee` does.
    pub fn fees(
        &self,
        feerate_sat_per_kw: u32,
        requested_sat: u64,
        contributed_sat: u64,
        is_channel_creation: bool,
    ) -> Fees {
        let mining_fee_sat = u64::from(feerate_sat_per_kw) * u64::from(self.funding_weight) / 1000;
        let proportional_sat = requested_sat
            .min(contributed_sat)
            .saturating_mul(u64::from(self.fee_proportional_bps))
            / 10_000;
        let flat_sat = u64::from(self.fee_base_sat)
            + if is_channel_creation {
                u64::from(self.channel_creation_fee_sat)
            } else {
                0
            };
        Fees {
            mining_fee_sat,
            service_fee_sat: flat_sat.saturating_add(proportional_sat),
        }
    }
}

impl Writeable for FundingRate {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.min_amount_sat.write(w)?;
        self.max_amount_sat.write(w)?;
        self.funding_weight.write(w)?;
        self.fee_proportional_bps.write(w)?;
        self.fee_base_sat.write(w)?;
        self.channel_creation_fee_sat.write(w)
    }
}

impl Readable for FundingRate {
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            min_amount_sat: Readable::read(r)?,
            max_amount_sat: Readable::read(r)?,
            funding_weight: Readable::read(r)?,
            fee_proportional_bps: Readable::read(r)?,
            fee_base_sat: Readable::read(r)?,
            channel_creation_fee_sat: Readable::read(r)?,
        })
    }
}

/// How a purchase is paid for, as a bit in the seller's payment type
/// bitfield.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaymentType {
    /// Bit 0: paid from the buyer's channel balance.
    FromChannelBalance,
    /// Bit 128: paid from HTLCs the seller relays later.
    FromFutureHtlc,
    /// Bit 129: like 128, but the buyer reveals the preimages up front.
    FromFutureHtlcWithPreimage,
    /// Bit 130: paid from channel balance, for HTLCs relayed later.
    FromChannelBalanceForFutureHtlc,
    Unknown(u32),
}

impl PaymentType {
    pub fn bit(&self) -> u32 {
        match self {
            Self::FromChannelBalance => 0,
            Self::FromFutureHtlc => 128,
            Self::FromFutureHtlcWithPreimage => 129,
            Self::FromChannelBalanceForFutureHtlc => 130,
            Self::Unknown(bit) => *bit,
        }
    }

    pub fn from_bit(bit: u32) -> Self {
        match bit {
            0 => Self::FromChannelBalance,
            128 => Self::FromFutureHtlc,
            129 => Self::FromFutureHtlcWithPreimage,
            130 => Self::FromChannelBalanceForFutureHtlc,
            bit => Self::Unknown(bit),
        }
    }

    /// Encode as the right-aligned bitfield of BOLT 9 features: bit 0 is the
    /// least significant bit of the last byte.
    pub fn encode(types: &[PaymentType]) -> Vec<u8> {
        let Some(max_bit) = types.iter().map(PaymentType::bit).max() else {
            return Vec::new();
        };
        let len = max_bit as usize / 8 + 1;
        let mut bytes = vec![0u8; len];
        for payment_type in types {
            let bit = payment_type.bit() as usize;
            bytes[len - 1 - bit / 8] |= 1 << (bit % 8);
        }
        bytes
    }

    /// Decode the bitfield written by [`PaymentType::encode`], lowest bit
    /// first.
    pub fn decode(bytes: &[u8]) -> Vec<PaymentType> {
        let mut types = Vec::new();
        for (byte_index, byte) in bytes.iter().rev().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    types.push(Self::from_bit((byte_index * 8 + bit) as u32));
                }
            }
        }
        types
    }
}

/// The `payment_details` of a funding request: the payment type plus the
/// hashes (or preimages) the fee will be collected from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaymentDetails {
    FromChannelBalance,
    FromFutureHtlc { payment_hashes: Vec<PaymentHash> },
    FromFutureHtlcWithPreimage { preimages: Vec<PaymentPreimage> },
    FromChannelBalanceForFutureHtlc { payment_hashes: Vec<PaymentHash> },
}

impl PaymentDetails {
    pub fn payment_type(&self) -> PaymentType {
        match self {
            Self::FromChannelBalance => PaymentType::FromChannelBalance,
            Self::FromFutureHtlc { .. } => PaymentType::FromFutureHtlc,
            Self::FromFutureHtlcWithPreimage { .. } => PaymentType::FromFutureHtlcWithPreimage,
            Self::FromChannelBalanceForFutureHtlc { .. } => {
                PaymentType::FromChannelBalanceForFutureHtlc
            }
        }
    }
}

fn read_32_byte_items<R: Read>(r: &mut R, len: u64) -> Result<Vec<[u8; 32]>, DecodeError> {
    if !len.is_multiple_of(32) {
        return Err(DecodeError::InvalidValue);
    }
    let mut items = Vec::new();
    for _ in 0..len / 32 {
        items.push(Readable::read(r)?);
    }
    Ok(items)
}

impl Writeable for PaymentDetails {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        BigSize(u64::from(self.payment_type().bit())).write(w)?;
        let items: Vec<&[u8; 32]> = match self {
            Self::FromChannelBalance => Vec::new(),
            Self::FromFutureHtlc { payment_hashes }
            | Self::FromChannelBalanceForFutureHtlc { payment_hashes } => {
                payment_hashes.iter().map(|hash| &hash.0).collect()
            }
            Self::FromFutureHtlcWithPreimage { preimages } => {
                preimages.iter().map(|preimage| &preimage.0).collect()
            }
        };
        BigSize(32 * items.len() as u64).write(w)?;
        for item in items {
            w.write_all(item)?;
        }
        Ok(())
    }
}

impl Readable for PaymentDetails {
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let tag = BigSize::read(r)?.0;
        let len = BigSize::read(r)?.0;
        match tag {
            0 => {
                if len != 0 {
                    return Err(DecodeError::InvalidValue);
                }
                Ok(Self::FromChannelBalance)
            }
            128 => Ok(Self::FromFutureHtlc {
                payment_hashes: read_32_byte_items(r, len)?
                    .into_iter()
                    .map(PaymentHash)
                    .collect(),
            }),
            129 => Ok(Self::FromFutureHtlcWithPreimage {
                preimages: read_32_byte_items(r, len)?
                    .into_iter()
                    .map(PaymentPreimage)
                    .collect(),
            }),
            130 => Ok(Self::FromChannelBalanceForFutureHtlc {
                payment_hashes: read_32_byte_items(r, len)?
                    .into_iter()
                    .map(PaymentHash)
                    .collect(),
            }),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// The seller's `will_fund_rates`: its rate card and the payment types it
/// accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WillFundRates {
    pub funding_rates: Vec<FundingRate>,
    pub payment_types: Vec<PaymentType>,
}

impl WillFundRates {
    /// The rate covering `amount_sat`, if the seller sells that much.
    pub fn find_rate(&self, amount_sat: u64) -> Option<&FundingRate> {
        self.funding_rates.iter().find(|rate| {
            u64::from(rate.min_amount_sat) <= amount_sat
                && amount_sat <= u64::from(rate.max_amount_sat)
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        Readable::read(&mut io::Cursor::new(bytes))
    }
}

impl Writeable for WillFundRates {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        (self.funding_rates.len() as u16).write(w)?;
        for rate in &self.funding_rates {
            rate.write(w)?;
        }
        let payment_types = PaymentType::encode(&self.payment_types);
        (payment_types.len() as u16).write(w)?;
        w.write_all(&payment_types)
    }
}

impl Readable for WillFundRates {
    fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let count: u16 = Readable::read(r)?;
        let mut funding_rates = Vec::new();
        for _ in 0..count {
            funding_rates.push(FundingRate::read(r)?);
        }
        let len: u16 = Readable::read(r)?;
        let mut payment_types = vec![0u8; usize::from(len)];
        r.read_exact(&mut payment_types)?;
        Ok(Self {
            funding_rates,
            payment_types: PaymentType::decode(&payment_types),
        })
    }
}

#[cfg(test)]
mod tests {
    use lampo_common::hex;

    use super::*;

    fn rate() -> FundingRate {
        FundingRate {
            min_amount_sat: 10_000,
            max_amount_sat: 1_000_000,
            funding_weight: 400,
            fee_proportional_bps: 100,
            fee_base_sat: 1_000,
            channel_creation_fee_sat: 2_000,
        }
    }

    const RATE_HEX: &str = "00002710 000f4240 0190 0064 000003e8 000007d0";

    fn unhex(raw: &str) -> Vec<u8> {
        hex::decode(raw.replace(' ', "")).unwrap()
    }

    #[test]
    fn funding_rate_round_trips() {
        let bytes = unhex(RATE_HEX);
        assert_eq!(rate().encode(), bytes);
        let decoded: FundingRate = Readable::read(&mut io::Cursor::new(&bytes)).unwrap();
        assert_eq!(decoded, rate());
    }

    #[test]
    fn fees_follow_the_rate_card() {
        // 5000 sat/kw * 400 weight / 1000 = 2000 sat mining fee.
        let fees = rate().fees(5_000, 100_000, 50_000, false);
        assert_eq!(fees.mining_fee_sat, 2_000);
        // 1000 base + min(100_000, 50_000) * 100 bps = 1000 + 500.
        assert_eq!(fees.service_fee_sat, 1_500);
        assert_eq!(fees.total_sat(), 3_500);
        assert_eq!(fees.total_msat(), 3_500_000);

        let creation = rate().fees(5_000, 100_000, 50_000, true);
        assert_eq!(creation.service_fee_sat, 3_500);

        // weight2fee truncates: 1234 * 400 / 1000 = 493.6 -> 493.
        assert_eq!(rate().fees(1_234, 0, 0, false).mining_fee_sat, 493);
    }

    #[test]
    fn payment_type_bitfield_is_right_aligned() {
        let future = [
            PaymentType::FromFutureHtlc,
            PaymentType::FromFutureHtlcWithPreimage,
            PaymentType::FromChannelBalanceForFutureHtlc,
        ];
        let mut expected = vec![0u8; 17];
        expected[0] = 0x07;
        assert_eq!(PaymentType::encode(&future), expected);
        assert_eq!(PaymentType::decode(&expected), future.to_vec());

        let mixed = [PaymentType::FromChannelBalance, PaymentType::FromFutureHtlc];
        let mut expected = vec![0u8; 17];
        expected[0] = 0x01;
        expected[16] = 0x01;
        assert_eq!(PaymentType::encode(&mixed), expected);
        assert_eq!(PaymentType::decode(&expected), mixed.to_vec());

        assert_eq!(PaymentType::encode(&[]), Vec::<u8>::new());
        assert_eq!(PaymentType::decode(&[0x02]), vec![PaymentType::Unknown(1)]);
        assert_eq!(PaymentType::from_bit(129).bit(), 129);
    }

    #[test]
    fn will_fund_rates_round_trips() {
        let rates = WillFundRates {
            funding_rates: vec![rate()],
            payment_types: vec![
                PaymentType::FromFutureHtlc,
                PaymentType::FromFutureHtlcWithPreimage,
                PaymentType::FromChannelBalanceForFutureHtlc,
            ],
        };
        let hex = format!("0001 {RATE_HEX} 0011 07 {}", "00".repeat(16));
        let bytes = unhex(&hex);
        assert_eq!(rates.encode(), bytes);
        assert_eq!(WillFundRates::decode(&bytes).unwrap(), rates);

        assert_eq!(rates.find_rate(9_999), None);
        assert_eq!(rates.find_rate(10_000), Some(&rate()));
        assert_eq!(rates.find_rate(1_000_000), Some(&rate()));
        assert_eq!(rates.find_rate(1_000_001), None);

        assert!(WillFundRates::decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn payment_details_round_trip() {
        let hash = PaymentHash([0x11; 32]);
        let preimage = PaymentPreimage([0x22; 32]);
        let cases = [
            (PaymentDetails::FromChannelBalance, "0000".to_owned()),
            (
                PaymentDetails::FromFutureHtlc {
                    payment_hashes: vec![hash],
                },
                format!("80 20 {}", "11".repeat(32)),
            ),
            (
                PaymentDetails::FromFutureHtlcWithPreimage {
                    preimages: vec![preimage],
                },
                format!("81 20 {}", "22".repeat(32)),
            ),
            (
                PaymentDetails::FromChannelBalanceForFutureHtlc {
                    payment_hashes: vec![hash, hash],
                },
                format!("82 40 {}", "11".repeat(64)),
            ),
        ];
        for (details, hex) in cases {
            let bytes = unhex(&hex);
            assert_eq!(details.encode(), bytes, "{details:?}");
            let decoded: PaymentDetails = Readable::read(&mut io::Cursor::new(&bytes)).unwrap();
            assert_eq!(decoded, details);
        }

        // A hash list whose length is not a multiple of 32 is invalid.
        let bad = unhex("80 21 00");
        assert!(<PaymentDetails as Readable>::read(&mut io::Cursor::new(&bad)).is_err());
        // Unknown payment types are rejected.
        let unknown = unhex("83 00");
        assert!(<PaymentDetails as Readable>::read(&mut io::Cursor::new(&unknown)).is_err());
    }
}
