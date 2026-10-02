//! Conversions between LDK types and the `vls-protocol` wire model.
use lampo_common::bitcoin::secp256k1::ecdsa::Signature;
use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::bitcoin::sighash::EcdsaSighashType;
use lampo_common::ldk::ln::chan_utils::HTLCOutputInCommitment;
use lampo_common::ldk::types::features::ChannelTypeFeatures;
use vls_protocol::model::{self, BitcoinSignature, Htlc, PubKey};
use vls_protocol::serde_bolt::Array;

/// LDK counts commitments down from here; VLS and CLN count up from zero.
pub const INITIAL_COMMITMENT_NUMBER: u64 = (1 << 48) - 1;

/// BOLT 9 feature bits VLS understands in `SetupChannel::channel_type`.
const OPT_STATIC_REMOTEKEY: usize = 12;
const OPT_ANCHOR_OUTPUTS: usize = 20;
const OPT_ANCHORS_ZERO_FEE_HTLC_TX: usize = 22;
const OPT_MAX: usize = 32;

pub fn commitment_number(ldk_idx: u64) -> u64 {
    INITIAL_COMMITMENT_NUMBER - ldk_idx
}

/// VLS keys a channel by a `dbid`; LDK's `channel_keys_id` carries it in the
/// last eight bytes, little endian, after 24 zero bytes.
pub fn keys_id_from_dbid(dbid: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[24..].copy_from_slice(&dbid.to_le_bytes());
    id
}

pub fn dbid_from_keys_id(keys_id: &[u8; 32]) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&keys_id[24..]);
    u64::from_le_bytes(bytes)
}

pub fn to_pubkey(key: &PublicKey) -> PubKey {
    PubKey(key.serialize())
}

pub fn from_pubkey(key: &PubKey) -> Result<PublicKey, ()> {
    PublicKey::from_slice(&key.0).map_err(|_| ())
}

pub fn from_signature(sig: &model::Signature) -> Result<Signature, ()> {
    Signature::from_compact(&sig.0).map_err(|_| ())
}

pub fn to_bitcoin_sig(sig: &Signature) -> BitcoinSignature {
    BitcoinSignature {
        signature: model::Signature(sig.serialize_compact()),
        sighash: EcdsaSighashType::All as u8,
    }
}

/// `is_remote` is true when the HTLC list describes the counterparty's
/// commitment; VLS wants the side that offered each HTLC from its own view.
pub fn to_htlcs(htlcs: &[HTLCOutputInCommitment], is_remote: bool) -> Array<Htlc> {
    Array(
        htlcs
            .iter()
            .map(|htlc| Htlc {
                side: if htlc.offered != is_remote {
                    Htlc::LOCAL
                } else {
                    Htlc::REMOTE
                },
                amount: htlc.amount_msat,
                payment_hash: model::Sha256(htlc.payment_hash.0),
                ctlv_expiry: htlc.cltv_expiry,
            })
            .collect(),
    )
}

/// Feature-bit encoding of the channel type, as `vls-protocol-signer`
/// produces it: bit `i` lands in byte `len - 1 - i / 8` at position `i % 8`.
/// Non-zero-fee anchors are encoded too; the signer rejects them itself
/// under `policy-channel-safe-type`.
pub fn channel_type_bytes(features: &ChannelTypeFeatures) -> Vec<u8> {
    let mut bits = vec![OPT_STATIC_REMOTEKEY];
    if features.supports_anchors_zero_fee_htlc_tx() {
        bits.push(OPT_ANCHOR_OUTPUTS);
        bits.push(OPT_ANCHORS_ZERO_FEE_HTLC_TX);
    } else if features.supports_anchors_nonzero_fee_htlc_tx() {
        bits.push(OPT_ANCHOR_OUTPUTS);
    }
    let len = OPT_MAX / 8;
    let mut out = vec![0u8; len];
    for bit in bits {
        out[len - 1 - bit / 8] |= 1 << (bit % 8);
    }
    out
}

/// zbase32, the alphabet lnd and CLN use for signed messages.
const ZBASE32: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

pub fn zbase32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ZBASE32[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ZBASE32[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// `sign_message` wire format: recovery id + 31, then the compact signature,
/// zbase32 encoded. Matches `encode_signed_message` in `vls-core`.
pub fn encode_signed_message(sig_and_recid: &[u8; 65]) -> String {
    let mut sigrec = Vec::with_capacity(65);
    sigrec.push(sig_and_recid[64] + 31);
    sigrec.extend_from_slice(&sig_and_recid[..64]);
    zbase32_encode(&sigrec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbid_round_trips_through_channel_keys_id() {
        let id = keys_id_from_dbid(0x0102_0304_0506_0708);
        assert_eq!(&id[..24], &[0u8; 24]);
        assert_eq!(dbid_from_keys_id(&id), 0x0102_0304_0506_0708);
    }

    #[test]
    fn channel_type_bits_match_the_signer_encoding() {
        // static_remotekey only: bit 12 -> byte 2, position 4.
        assert_eq!(
            channel_type_bytes(&ChannelTypeFeatures::only_static_remote_key()),
            vec![0x00, 0x00, 0x10, 0x00]
        );
        // zero-fee anchors: bits 12, 20 and 22.
        assert_eq!(
            channel_type_bytes(&ChannelTypeFeatures::anchors_zero_htlc_fee_and_dependencies()),
            vec![0x00, 0x50, 0x10, 0x00]
        );
    }

    #[test]
    fn zbase32_matches_the_reference_alphabet() {
        // 0b00100_100 -> index 4 ('r'), then 100 padded to 10000 -> 16 ('o').
        assert_eq!(zbase32_encode(&[0x24]), "ro");
        // python-zbase32 reference vector.
        assert_eq!(zbase32_encode(b"hello"), "pb1sa5dx");
    }
}
