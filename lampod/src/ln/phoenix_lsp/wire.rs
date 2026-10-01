//! Wire format of the Phoenix LSP extension messages, as lightning-kmp
//! 1.13.2 writes them: on-the-fly funding (bLIP 36, types 41041 to 41044),
//! fee credit (bLIP 41, 41045 and 41046), `recommended_feerates` (39409)
//! and the BIP 353 address request (35025 and 35027).
//!
//! All integers are big-endian. Three of the types are even (41042, 41044,
//! 41046): the reader must own them, or the peer manager treats them as
//! unknown even messages and disconnects the LSP.

use lampo_common::bitcoin::constants::ChainHash;
use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::ldk::io::{self, Read};
use lampo_common::ldk::ln::msgs::DecodeError;
use lampo_common::ldk::ln::types::ChannelId;
use lampo_common::ldk::ln::wire::Type;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::util::ser::{BigSize, LengthLimitedRead, Readable, Writeable, Writer};

pub const RECOMMENDED_FEERATES_TYPE: u16 = 39409;
pub const WILL_ADD_HTLC_TYPE: u16 = 41041;
pub const WILL_FAIL_HTLC_TYPE: u16 = 41042;
pub const WILL_FAIL_MALFORMED_HTLC_TYPE: u16 = 41043;
pub const CANCEL_ON_THE_FLY_FUNDING_TYPE: u16 = 41044;
pub const ADD_FEE_CREDIT_TYPE: u16 = 41045;
pub const CURRENT_FEE_CREDIT_TYPE: u16 = 41046;
pub const DNS_ADDRESS_REQUEST_TYPE: u16 = 35025;
pub const DNS_ADDRESS_RESPONSE_TYPE: u16 = 35027;

/// Version byte, 33-byte ephemeral key, 1300-byte payload, 32-byte HMAC.
pub const ONION_PACKET_LEN: usize = 1366;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeerateRange {
    pub min: u32,
    pub max: u32,
}

/// Type 39409: the feerates the LSP wants for funding and commitment
/// transactions, in sat per kw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecommendedFeerates {
    pub chain_hash: ChainHash,
    pub funding_feerate: u32,
    pub commitment_feerate: u32,
    /// TLV 1.
    pub funding_feerate_range: Option<FeerateRange>,
    /// TLV 3.
    pub commitment_feerate_range: Option<FeerateRange>,
}

/// Type 41041: an HTLC the LSP would relay if this node buys liquidity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WillAddHtlc {
    pub chain_hash: ChainHash,
    pub id: [u8; 32],
    pub amount_msat: u64,
    pub payment_hash: PaymentHash,
    pub cltv_expiry: u32,
    pub onion_routing_packet: Box<[u8; ONION_PACKET_LEN]>,
    /// TLV 0: the blinding point when the HTLC is part of a blinded path.
    pub path_key: Option<PublicKey>,
}

/// Type 41042: this node declines a `will_add_htlc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WillFailHtlc {
    pub id: [u8; 32],
    pub payment_hash: PaymentHash,
    pub reason: Vec<u8>,
}

/// Type 41043: this node could not decode the onion of a `will_add_htlc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WillFailMalformedHtlc {
    pub id: [u8; 32],
    pub payment_hash: PaymentHash,
    pub sha256_of_onion: [u8; 32],
    pub failure_code: u16,
}

/// Type 41044: the LSP gave up on funding for these payments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelOnTheFlyFunding {
    pub channel_id: ChannelId,
    pub payment_hashes: Vec<PaymentHash>,
    /// ASCII text.
    pub reason: Vec<u8>,
}

impl CancelOnTheFlyFunding {
    pub fn reason_str(&self) -> String {
        String::from_utf8_lossy(&self.reason).into_owned()
    }
}

/// Type 41045: this node lets the LSP keep a payment as fee credit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddFeeCredit {
    pub chain_hash: ChainHash,
    pub preimage: PaymentPreimage,
}

/// Type 41046: the fee credit the LSP holds for this node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentFeeCredit {
    pub chain_hash: ChainHash,
    pub amount_msat: u64,
}

/// Type 35025: ask the LSP to publish a BIP 353 address for an offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsAddressRequest {
    pub chain_hash: ChainHash,
    /// The offer's TLV stream.
    pub offer: Vec<u8>,
    /// BCP 47 language tag, UTF-8.
    pub language: String,
}

/// Type 35027: the address the LSP published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsAddressResponse {
    pub chain_hash: ChainHash,
    pub address: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhoenixLspMessage {
    RecommendedFeerates(RecommendedFeerates),
    WillAddHtlc(WillAddHtlc),
    WillFailHtlc(WillFailHtlc),
    WillFailMalformedHtlc(WillFailMalformedHtlc),
    CancelOnTheFlyFunding(CancelOnTheFlyFunding),
    AddFeeCredit(AddFeeCredit),
    CurrentFeeCredit(CurrentFeeCredit),
    DnsAddressRequest(DnsAddressRequest),
    DnsAddressResponse(DnsAddressResponse),
}

impl PhoenixLspMessage {
    /// The message name, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Self::RecommendedFeerates(_) => "recommended_feerates",
            Self::WillAddHtlc(_) => "will_add_htlc",
            Self::WillFailHtlc(_) => "will_fail_htlc",
            Self::WillFailMalformedHtlc(_) => "will_fail_malformed_htlc",
            Self::CancelOnTheFlyFunding(_) => "cancel_on_the_fly_funding",
            Self::AddFeeCredit(_) => "add_fee_credit",
            Self::CurrentFeeCredit(_) => "current_fee_credit",
            Self::DnsAddressRequest(_) => "dns_address_request",
            Self::DnsAddressResponse(_) => "dns_address_response",
        }
    }
}

impl Type for PhoenixLspMessage {
    fn type_id(&self) -> u16 {
        match self {
            Self::RecommendedFeerates(_) => RECOMMENDED_FEERATES_TYPE,
            Self::WillAddHtlc(_) => WILL_ADD_HTLC_TYPE,
            Self::WillFailHtlc(_) => WILL_FAIL_HTLC_TYPE,
            Self::WillFailMalformedHtlc(_) => WILL_FAIL_MALFORMED_HTLC_TYPE,
            Self::CancelOnTheFlyFunding(_) => CANCEL_ON_THE_FLY_FUNDING_TYPE,
            Self::AddFeeCredit(_) => ADD_FEE_CREDIT_TYPE,
            Self::CurrentFeeCredit(_) => CURRENT_FEE_CREDIT_TYPE,
            Self::DnsAddressRequest(_) => DNS_ADDRESS_REQUEST_TYPE,
            Self::DnsAddressResponse(_) => DNS_ADDRESS_RESPONSE_TYPE,
        }
    }
}

impl Writeable for PhoenixLspMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        match self {
            Self::RecommendedFeerates(msg) => msg.write(w),
            Self::WillAddHtlc(msg) => msg.write(w),
            Self::WillFailHtlc(msg) => msg.write(w),
            Self::WillFailMalformedHtlc(msg) => msg.write(w),
            Self::CancelOnTheFlyFunding(msg) => msg.write(w),
            Self::AddFeeCredit(msg) => msg.write(w),
            Self::CurrentFeeCredit(msg) => msg.write(w),
            Self::DnsAddressRequest(msg) => msg.write(w),
            Self::DnsAddressResponse(msg) => msg.write(w),
        }
    }
}

/// Decode the payload of `message_type`, or `None` when the type is not a
/// Phoenix message. The two-byte type has already been consumed.
pub fn read<R: LengthLimitedRead>(
    message_type: u16,
    r: &mut R,
) -> Result<Option<PhoenixLspMessage>, DecodeError> {
    let msg = match message_type {
        RECOMMENDED_FEERATES_TYPE => {
            PhoenixLspMessage::RecommendedFeerates(RecommendedFeerates::read(r)?)
        }
        WILL_ADD_HTLC_TYPE => PhoenixLspMessage::WillAddHtlc(WillAddHtlc::read(r)?),
        WILL_FAIL_HTLC_TYPE => PhoenixLspMessage::WillFailHtlc(WillFailHtlc::read(r)?),
        WILL_FAIL_MALFORMED_HTLC_TYPE => {
            PhoenixLspMessage::WillFailMalformedHtlc(WillFailMalformedHtlc::read(r)?)
        }
        CANCEL_ON_THE_FLY_FUNDING_TYPE => {
            PhoenixLspMessage::CancelOnTheFlyFunding(CancelOnTheFlyFunding::read(r)?)
        }
        ADD_FEE_CREDIT_TYPE => PhoenixLspMessage::AddFeeCredit(AddFeeCredit::read(r)?),
        CURRENT_FEE_CREDIT_TYPE => PhoenixLspMessage::CurrentFeeCredit(CurrentFeeCredit::read(r)?),
        DNS_ADDRESS_REQUEST_TYPE => {
            PhoenixLspMessage::DnsAddressRequest(DnsAddressRequest::read(r)?)
        }
        DNS_ADDRESS_RESPONSE_TYPE => {
            PhoenixLspMessage::DnsAddressResponse(DnsAddressResponse::read(r)?)
        }
        _ => return Ok(None),
    };
    Ok(Some(msg))
}

fn write_bytes_u16<W: Writer>(w: &mut W, bytes: &[u8]) -> Result<(), io::Error> {
    let len =
        u16::try_from(bytes.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    len.write(w)?;
    w.write_all(bytes)
}

fn read_bytes_u16<R: Read>(r: &mut R) -> Result<Vec<u8>, DecodeError> {
    let len: u16 = Readable::read(r)?;
    let mut bytes = vec![0u8; usize::from(len)];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_utf8_u16<R: Read>(r: &mut R) -> Result<String, DecodeError> {
    String::from_utf8(read_bytes_u16(r)?).map_err(|_| DecodeError::InvalidValue)
}

fn write_tlv<W: Writer>(w: &mut W, tlv_type: u64, value: &[u8]) -> Result<(), io::Error> {
    BigSize(tlv_type).write(w)?;
    BigSize(value.len() as u64).write(w)?;
    w.write_all(value)
}

/// Read a TLV stream up to the end of `r`, handing every record to
/// `on_record`, which returns whether it knew the type. Types must be
/// strictly increasing; an unknown even type is an error, an unknown odd
/// type is skipped (BOLT 1).
fn read_tlv_stream<R: LengthLimitedRead>(
    r: &mut R,
    mut on_record: impl FnMut(u64, &[u8]) -> Result<bool, DecodeError>,
) -> Result<(), DecodeError> {
    let mut last_type: Option<u64> = None;
    while r.remaining_bytes() > 0 {
        let tlv_type = BigSize::read(r)?.0;
        if last_type.is_some_and(|last| tlv_type <= last) {
            return Err(DecodeError::InvalidValue);
        }
        last_type = Some(tlv_type);
        let len = BigSize::read(r)?.0;
        if len > r.remaining_bytes() {
            return Err(DecodeError::BadLengthDescriptor);
        }
        let mut value = vec![0u8; len as usize];
        r.read_exact(&mut value)?;
        if !on_record(tlv_type, &value)? && tlv_type % 2 == 0 {
            return Err(DecodeError::UnknownRequiredFeature);
        }
    }
    Ok(())
}

impl FeerateRange {
    fn to_tlv_value(self) -> [u8; 8] {
        let mut value = [0u8; 8];
        value[..4].copy_from_slice(&self.min.to_be_bytes());
        value[4..].copy_from_slice(&self.max.to_be_bytes());
        value
    }

    fn from_tlv_value(value: &[u8]) -> Result<Self, DecodeError> {
        let value: &[u8; 8] = value.try_into().map_err(|_| DecodeError::InvalidValue)?;
        Ok(Self {
            min: u32::from_be_bytes([value[0], value[1], value[2], value[3]]),
            max: u32::from_be_bytes([value[4], value[5], value[6], value[7]]),
        })
    }
}

impl RecommendedFeerates {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.chain_hash.write(w)?;
        self.funding_feerate.write(w)?;
        self.commitment_feerate.write(w)?;
        if let Some(range) = self.funding_feerate_range {
            write_tlv(w, 1, &range.to_tlv_value())?;
        }
        if let Some(range) = self.commitment_feerate_range {
            write_tlv(w, 3, &range.to_tlv_value())?;
        }
        Ok(())
    }

    pub fn read<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        let chain_hash = Readable::read(r)?;
        let funding_feerate = Readable::read(r)?;
        let commitment_feerate = Readable::read(r)?;
        let mut funding_feerate_range = None;
        let mut commitment_feerate_range = None;
        read_tlv_stream(r, |tlv_type, value| match tlv_type {
            1 => {
                funding_feerate_range = Some(FeerateRange::from_tlv_value(value)?);
                Ok(true)
            }
            3 => {
                commitment_feerate_range = Some(FeerateRange::from_tlv_value(value)?);
                Ok(true)
            }
            _ => Ok(false),
        })?;
        Ok(Self {
            chain_hash,
            funding_feerate,
            commitment_feerate,
            funding_feerate_range,
            commitment_feerate_range,
        })
    }
}

impl WillAddHtlc {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.chain_hash.write(w)?;
        w.write_all(&self.id)?;
        self.amount_msat.write(w)?;
        self.payment_hash.write(w)?;
        self.cltv_expiry.write(w)?;
        w.write_all(&self.onion_routing_packet[..])?;
        if let Some(path_key) = self.path_key {
            write_tlv(w, 0, &path_key.serialize())?;
        }
        Ok(())
    }

    pub fn read<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        let chain_hash = Readable::read(r)?;
        let id = Readable::read(r)?;
        let amount_msat = Readable::read(r)?;
        let payment_hash = Readable::read(r)?;
        let cltv_expiry = Readable::read(r)?;
        let mut onion_routing_packet = Box::new([0u8; ONION_PACKET_LEN]);
        r.read_exact(&mut onion_routing_packet[..])?;
        let mut path_key = None;
        read_tlv_stream(r, |tlv_type, value| match tlv_type {
            0 => {
                path_key =
                    Some(PublicKey::from_slice(value).map_err(|_| DecodeError::InvalidValue)?);
                Ok(true)
            }
            _ => Ok(false),
        })?;
        Ok(Self {
            chain_hash,
            id,
            amount_msat,
            payment_hash,
            cltv_expiry,
            onion_routing_packet,
            path_key,
        })
    }
}

impl WillFailHtlc {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        w.write_all(&self.id)?;
        self.payment_hash.write(w)?;
        write_bytes_u16(w, &self.reason)
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            id: Readable::read(r)?,
            payment_hash: Readable::read(r)?,
            reason: read_bytes_u16(r)?,
        })
    }
}

impl WillFailMalformedHtlc {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        w.write_all(&self.id)?;
        self.payment_hash.write(w)?;
        w.write_all(&self.sha256_of_onion)?;
        self.failure_code.write(w)
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            id: Readable::read(r)?,
            payment_hash: Readable::read(r)?,
            sha256_of_onion: Readable::read(r)?,
            failure_code: Readable::read(r)?,
        })
    }
}

impl CancelOnTheFlyFunding {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.channel_id.write(w)?;
        let count = u16::try_from(self.payment_hashes.len())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        count.write(w)?;
        for payment_hash in &self.payment_hashes {
            payment_hash.write(w)?;
        }
        write_bytes_u16(w, &self.reason)
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        let channel_id = Readable::read(r)?;
        let count: u16 = Readable::read(r)?;
        let mut payment_hashes = Vec::new();
        for _ in 0..count {
            payment_hashes.push(Readable::read(r)?);
        }
        Ok(Self {
            channel_id,
            payment_hashes,
            reason: read_bytes_u16(r)?,
        })
    }
}

impl AddFeeCredit {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.chain_hash.write(w)?;
        self.preimage.write(w)
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            chain_hash: Readable::read(r)?,
            preimage: Readable::read(r)?,
        })
    }
}

impl CurrentFeeCredit {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.chain_hash.write(w)?;
        self.amount_msat.write(w)
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            chain_hash: Readable::read(r)?,
            amount_msat: Readable::read(r)?,
        })
    }
}

impl DnsAddressRequest {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.chain_hash.write(w)?;
        write_bytes_u16(w, &self.offer)?;
        write_bytes_u16(w, self.language.as_bytes())
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            chain_hash: Readable::read(r)?,
            offer: read_bytes_u16(r)?,
            language: read_utf8_u16(r)?,
        })
    }
}

impl DnsAddressResponse {
    pub fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.chain_hash.write(w)?;
        write_bytes_u16(w, self.address.as_bytes())
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            chain_hash: Readable::read(r)?,
            address: read_utf8_u16(r)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use lampo_common::hex;

    use super::*;

    const CHAIN: [u8; 32] = [0x11; 32];
    const PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    fn unhex(raw: &str) -> Vec<u8> {
        hex::decode(raw.replace([' ', '\n'], "")).unwrap()
    }

    fn chain() -> String {
        "11".repeat(32)
    }

    /// `read` must decode `hex` into `expected`, and `expected` must encode
    /// back to `hex` with the right type id.
    fn round_trip(message_type: u16, hex: &str, expected: PhoenixLspMessage) {
        let bytes = unhex(hex);
        let decoded = read(message_type, &mut &bytes[..])
            .unwrap()
            .unwrap_or_else(|| panic!("type {message_type} is a Phoenix message"));
        assert_eq!(decoded, expected);
        assert_eq!(expected.type_id(), message_type);
        assert_eq!(expected.encode(), bytes);
    }

    #[test]
    fn unknown_types_are_not_ours() {
        assert_eq!(read(35026, &mut &[0u8; 4][..]).unwrap(), None);
        assert_eq!(read(65535, &mut &[][..]).unwrap(), None);
        assert_eq!(read(16, &mut &[][..]).unwrap(), None);
    }

    #[test]
    fn recommended_feerates() {
        let hex = format!(
            "{} 000003e8 000001f4 01 08 000003e8 00002710 03 08 000001f4 00000bb8",
            chain()
        );
        round_trip(
            RECOMMENDED_FEERATES_TYPE,
            &hex,
            PhoenixLspMessage::RecommendedFeerates(RecommendedFeerates {
                chain_hash: ChainHash::from(CHAIN),
                funding_feerate: 1_000,
                commitment_feerate: 500,
                funding_feerate_range: Some(FeerateRange {
                    min: 1_000,
                    max: 10_000,
                }),
                commitment_feerate_range: Some(FeerateRange {
                    min: 500,
                    max: 3_000,
                }),
            }),
        );
        // No TLVs at all.
        round_trip(
            RECOMMENDED_FEERATES_TYPE,
            &format!("{} 000003e8 000001f4", chain()),
            PhoenixLspMessage::RecommendedFeerates(RecommendedFeerates {
                chain_hash: ChainHash::from(CHAIN),
                funding_feerate: 1_000,
                commitment_feerate: 500,
                funding_feerate_range: None,
                commitment_feerate_range: None,
            }),
        );
        // An unknown odd TLV is skipped, an unknown even one is an error,
        // and the stream must be ordered.
        let odd = unhex(&format!("{} 000003e8 000001f4 05 01 ff", chain()));
        assert!(read(RECOMMENDED_FEERATES_TYPE, &mut &odd[..])
            .unwrap()
            .is_some());
        let even = unhex(&format!("{} 000003e8 000001f4 04 01 ff", chain()));
        assert!(read(RECOMMENDED_FEERATES_TYPE, &mut &even[..]).is_err());
        let unordered = unhex(&format!(
            "{} 000003e8 000001f4 03 08 000001f4 00000bb8 01 08 000003e8 00002710",
            chain()
        ));
        assert!(read(RECOMMENDED_FEERATES_TYPE, &mut &unordered[..]).is_err());
        let truncated = unhex(&format!("{} 000003e8 000001f4 01 08 0000", chain()));
        assert!(read(RECOMMENDED_FEERATES_TYPE, &mut &truncated[..]).is_err());
    }

    #[test]
    fn will_add_htlc() {
        let onion = "44".repeat(ONION_PACKET_LEN);
        let base = format!(
            "{} {} 00000000000f4240 {} 00000190 {onion}",
            chain(),
            "22".repeat(32),
            "33".repeat(32)
        );
        let expected = WillAddHtlc {
            chain_hash: ChainHash::from(CHAIN),
            id: [0x22; 32],
            amount_msat: 1_000_000,
            payment_hash: PaymentHash([0x33; 32]),
            cltv_expiry: 400,
            onion_routing_packet: Box::new([0x44; ONION_PACKET_LEN]),
            path_key: None,
        };
        round_trip(
            WILL_ADD_HTLC_TYPE,
            &base,
            PhoenixLspMessage::WillAddHtlc(expected.clone()),
        );
        round_trip(
            WILL_ADD_HTLC_TYPE,
            &format!("{base} 00 21 {PUBKEY}"),
            PhoenixLspMessage::WillAddHtlc(WillAddHtlc {
                path_key: Some(PublicKey::from_slice(&unhex(PUBKEY)).unwrap()),
                ..expected
            }),
        );
        // A short onion is a short read, not a panic.
        let short = unhex(&base[..base.len() - 2]);
        assert!(read(WILL_ADD_HTLC_TYPE, &mut &short[..]).is_err());
    }

    #[test]
    fn will_fail_htlc() {
        round_trip(
            WILL_FAIL_HTLC_TYPE,
            &format!("{} {} 0003 010203", "22".repeat(32), "33".repeat(32)),
            PhoenixLspMessage::WillFailHtlc(WillFailHtlc {
                id: [0x22; 32],
                payment_hash: PaymentHash([0x33; 32]),
                reason: vec![1, 2, 3],
            }),
        );
    }

    #[test]
    fn will_fail_malformed_htlc() {
        round_trip(
            WILL_FAIL_MALFORMED_HTLC_TYPE,
            &format!(
                "{} {} {} 4005",
                "22".repeat(32),
                "33".repeat(32),
                "55".repeat(32)
            ),
            PhoenixLspMessage::WillFailMalformedHtlc(WillFailMalformedHtlc {
                id: [0x22; 32],
                payment_hash: PaymentHash([0x33; 32]),
                sha256_of_onion: [0x55; 32],
                failure_code: 0x4005,
            }),
        );
    }

    #[test]
    fn cancel_on_the_fly_funding() {
        let reason = "no fees";
        round_trip(
            CANCEL_ON_THE_FLY_FUNDING_TYPE,
            &format!(
                "{} 0002 {} {} 0007 {}",
                "66".repeat(32),
                "33".repeat(32),
                "34".repeat(32),
                hex::encode(reason)
            ),
            PhoenixLspMessage::CancelOnTheFlyFunding(CancelOnTheFlyFunding {
                channel_id: ChannelId([0x66; 32]),
                payment_hashes: vec![PaymentHash([0x33; 32]), PaymentHash([0x34; 32])],
                reason: reason.as_bytes().to_vec(),
            }),
        );
        let cancel = CancelOnTheFlyFunding {
            channel_id: ChannelId([0x66; 32]),
            payment_hashes: Vec::new(),
            reason: reason.as_bytes().to_vec(),
        };
        assert_eq!(cancel.reason_str(), reason);
    }

    #[test]
    fn fee_credit_messages() {
        round_trip(
            ADD_FEE_CREDIT_TYPE,
            &format!("{} {}", chain(), "77".repeat(32)),
            PhoenixLspMessage::AddFeeCredit(AddFeeCredit {
                chain_hash: ChainHash::from(CHAIN),
                preimage: PaymentPreimage([0x77; 32]),
            }),
        );
        round_trip(
            CURRENT_FEE_CREDIT_TYPE,
            &format!("{} 0000000000000bb8", chain()),
            PhoenixLspMessage::CurrentFeeCredit(CurrentFeeCredit {
                chain_hash: ChainHash::from(CHAIN),
                amount_msat: 3_000,
            }),
        );
    }

    #[test]
    fn dns_address_messages() {
        round_trip(
            DNS_ADDRESS_REQUEST_TYPE,
            &format!("{} 0003 aabbcc 0002 656e", chain()),
            PhoenixLspMessage::DnsAddressRequest(DnsAddressRequest {
                chain_hash: ChainHash::from(CHAIN),
                offer: vec![0xaa, 0xbb, 0xcc],
                language: "en".to_owned(),
            }),
        );
        let address = "alice@phoenix.io";
        round_trip(
            DNS_ADDRESS_RESPONSE_TYPE,
            &format!("{} 0010 {}", chain(), hex::encode(address)),
            PhoenixLspMessage::DnsAddressResponse(DnsAddressResponse {
                chain_hash: ChainHash::from(CHAIN),
                address: address.to_owned(),
            }),
        );
        // Invalid UTF-8 in a string field is rejected.
        let bad = unhex(&format!("{} 0001 ff", chain()));
        assert!(read(DNS_ADDRESS_RESPONSE_TYPE, &mut &bad[..]).is_err());
    }
}
