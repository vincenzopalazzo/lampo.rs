//! Minimal implementation of the Eye of Satoshi (TEOS) client protocol.
//!
//! This mirrors the appointment types, blob encryption, and message
//! signing of `teos-common` (<https://github.com/talaia-labs/rust-teos>)
//! without depending on the crate itself: `teos-common` pins an older
//! `lightning` major version (which would split the LDK stack in two,
//! see issue #537) and drags in tonic/prost/rusqlite plus a `protoc`
//! build requirement for a gRPC surface lampo never touches. The wire
//! format implemented here is the tower's public HTTP JSON API, and it
//! is validated against `teos-common`'s own test vectors.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use serde::{Deserialize, Serialize};

use bitcoin::consensus;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::{PublicKey, SecretKey};
use bitcoin::{Transaction, Txid};
use lightning::util::message_signing;

/// Length in bytes of an appointment locator.
pub const LOCATOR_LEN: usize = 16;

/// The appointment identifier: the first 16 bytes of the dispute
/// (revoked commitment) txid.
#[derive(Debug, Eq, PartialEq, Copy, Clone, Hash)]
pub struct Locator([u8; LOCATOR_LEN]);

impl Locator {
    pub fn new(txid: Txid) -> Self {
        // SAFETY: a txid is 32 bytes, the slice is always 16 bytes.
        Locator(txid[..LOCATOR_LEN].try_into().unwrap())
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl std::fmt::Display for Locator {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

/// An appointment between a client and a tower: the locator plus the
/// encrypted penalty transaction.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Appointment {
    pub locator: Locator,
    pub encrypted_blob: Vec<u8>,
    pub to_self_delay: u32,
}

impl Appointment {
    pub fn new(locator: Locator, encrypted_blob: Vec<u8>, to_self_delay: u32) -> Self {
        Appointment {
            locator,
            encrypted_blob,
            to_self_delay,
        }
    }

    /// Serialization the user signs: `locator || encrypted_blob ||
    /// to_self_delay` (big endian), as defined by teos-common.
    pub fn to_vec(&self) -> Vec<u8> {
        let mut result = self.locator.to_vec();
        result.extend(&self.encrypted_blob);
        result.extend(self.to_self_delay.to_be_bytes());
        result
    }
}

/// Encrypts a penalty transaction with `chacha20poly1305`, keyed by
/// `sha256(dispute_txid)` with a zero IV, matching the tower side.
pub fn encrypt(penalty_tx: &Transaction, dispute_txid: &Txid) -> anyhow::Result<Vec<u8>> {
    let nonce = Nonce::default();
    let k = sha256::Hash::hash(dispute_txid.as_byte_array());
    let key = Key::from_slice(k.as_byte_array());

    let cypher = ChaCha20Poly1305::new(key);
    cypher
        .encrypt(&nonce, consensus::serialize(penalty_tx).as_ref())
        .map_err(|err| anyhow::anyhow!("cannot encrypt the penalty transaction: {err}"))
}

/// Signs a message with the lightning message signing scheme (zbase32),
/// the format towers verify.
pub fn sign(msg: &[u8], sk: &SecretKey) -> String {
    message_signing::sign(msg, sk)
}

/// Recovers the public key that signed `msg`.
pub fn recover_pk(msg: &[u8], sig: &str) -> anyhow::Result<PublicKey> {
    message_signing::recover_pk(msg, sig).map_err(|err| anyhow::anyhow!("{err}"))
}

// The tower HTTP API: JSON bodies mirroring teos-common's proto
// definitions, with `bytes` fields hex encoded.

/// `POST /register` request body.
#[derive(Debug, Serialize)]
pub struct RegisterRequest {
    #[serde(with = "hex::serde")]
    pub user_id: Vec<u8>,
}

/// `POST /register` response body.
#[derive(Debug, Deserialize)]
pub struct RegisterResponse {
    #[serde(with = "hex::serde")]
    pub user_id: Vec<u8>,
    pub available_slots: u32,
    pub subscription_start: u32,
    pub subscription_expiry: u32,
    pub subscription_signature: String,
}

impl RegisterResponse {
    /// Serialization of the registration receipt the tower signed:
    /// `user_id || available_slots || subscription_start ||
    /// subscription_expiry` (big endian).
    pub fn receipt_to_vec(&self) -> Vec<u8> {
        let mut ser = Vec::new();
        ser.extend_from_slice(&self.user_id);
        ser.extend_from_slice(&self.available_slots.to_be_bytes());
        ser.extend_from_slice(&self.subscription_start.to_be_bytes());
        ser.extend_from_slice(&self.subscription_expiry.to_be_bytes());
        ser
    }

    /// Verifies the subscription signature against the tower id.
    pub fn verify(&self, tower_id: &PublicKey) -> bool {
        recover_pk(&self.receipt_to_vec(), &self.subscription_signature)
            .map(|pk| pk == *tower_id)
            .unwrap_or(false)
    }
}

/// The appointment as it goes on the wire.
#[derive(Debug, Serialize)]
pub struct WireAppointment {
    #[serde(with = "hex::serde")]
    pub locator: Vec<u8>,
    #[serde(with = "hex::serde")]
    pub encrypted_blob: Vec<u8>,
    pub to_self_delay: u32,
}

/// `POST /add_appointment` request body.
#[derive(Debug, Serialize)]
pub struct AddAppointmentRequest {
    pub appointment: WireAppointment,
    pub signature: String,
}

/// `POST /add_appointment` response body.
#[derive(Debug, Deserialize)]
pub struct AddAppointmentResponse {
    #[serde(with = "hex::serde")]
    pub locator: Vec<u8>,
    pub start_block: u32,
    pub signature: String,
    pub available_slots: u32,
    pub subscription_expiry: u32,
}

impl AddAppointmentResponse {
    /// Serialization of the appointment receipt the tower signed:
    /// `user_signature || start_block` (big endian).
    pub fn receipt_to_vec(&self, user_signature: &str) -> Vec<u8> {
        let mut ser = Vec::new();
        ser.extend_from_slice(user_signature.as_bytes());
        ser.extend_from_slice(&self.start_block.to_be_bytes());
        ser
    }

    /// Verifies the tower receipt signature against the tower id.
    pub fn verify(&self, user_signature: &str, tower_id: &PublicKey) -> bool {
        recover_pk(&self.receipt_to_vec(user_signature), &self.signature)
            .map(|pk| pk == *tower_id)
            .unwrap_or(false)
    }
}

/// `POST /get_appointment` request body.
#[derive(Debug, Serialize)]
pub struct GetAppointmentRequest {
    #[serde(with = "hex::serde")]
    pub locator: Vec<u8>,
    pub signature: String,
}

/// Error body returned by the tower on a non-2xx response.
#[derive(Debug, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub error_code: u8,
}

/// Error codes from `teos_common::errors` the client reacts to.
pub mod errors {
    pub const INVALID_SIGNATURE_OR_SUBSCRIPTION_ERROR: u8 = 7;
    pub const APPOINTMENT_ALREADY_TRIGGERED: u8 = 35;
    pub const REGISTRATION_RESOURCE_EXHAUSTED: u8 = 65;
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    // Test vectors from teos-common's cryptography module.
    const HEX_TX: &str = "010000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff54038e830a1b4d696e656420627920416e74506f6f6c373432c2005b005e7a0ae3fabe6d6d7841cd582ead8ea5dd8e3de1173cae6fcd2a53c7362ebb7fb6f815604fe07cbe0200000000000000ac0e060005f90000ffffffff04d9476026000000001976a91411dbe48cc6b617f9c6adaf4d9ed5f625b1c7cb5988ac0000000000000000266a24aa21a9ed7248c6efddd8d99bfddd7f499f0b915bffa8253003cc934df1ff14a81301e2340000000000000000266a24b9e11b6d7054937e13f39529d6ad7e685e9dd4efa426f247d5f5a5bed58cdddb2d0fa60100000000000000002b6a2952534b424c4f434b3a054a68aa5368740e8b3e3c67bce45619c2cfd07d4d4f0936a5612d2d0034fa0a0120000000000000000000000000000000000000000000000000000000000000000000000000";
    const HEX_TXID: &str = "d6ac4a5e61657c4c604dcde855a1db74ec6b3e54f32695d72c5e11c7761ea1b4";
    const ENC_BLOB: &str = "f64d730654738fdbcd9e65068be17bc1abb44e74f8977985cce48e77209cf97292c862e4eb7190aedc6c53ceddda6871a3988d1d9608e2d0dd7a1f59769e410618a7029001479ac3b9d699b11a08b0ccb04e56bfee88461d9cd3207623a4a543996dd3805323c93cd62069636305aaf159e9cca1063ad1f097c16fb3c2ebbcf09be96512c5d7c195c684569cbe8b7979870b04cada9806b7610569c66021afcc63f46dd4af75716950c4de094334cdf7d9e532820afe29d2621dd79920c7e0ecc10853517dd84ca9d699f712c229e86954c227cba1d0fc87c8d48ac05e2de8a6bc980afdfafcd7064e411c8d76065c06cc7f233e869eaff5bd8ccb5d8f0090d91a8f017355cc115863356ecf06cdda9b309096ea766d033dbd4f70a789a5b03138cfc7e2900a79bb465abf07a7ac45c41b4b30c008d4b299aad9d001cf45acd07e47cdd63c3b13d4b0788b041735225b5db1a43a2142311f695478168e31deb260702976fd70d0724ded84a7c3f89b";

    #[test]
    fn encrypt_matches_teos_vector() {
        let tx: Transaction = consensus::deserialize(&hex::decode(HEX_TX).unwrap()).unwrap();
        let txid = Txid::from_str(HEX_TXID).unwrap();
        assert_eq!(encrypt(&tx, &txid).unwrap(), hex::decode(ENC_BLOB).unwrap());
    }

    #[test]
    fn locator_is_txid_prefix() {
        let txid = Txid::from_str(HEX_TXID).unwrap();
        let locator = Locator::new(txid);
        // Txid Display is big endian (reversed), the locator is over the
        // internal byte order.
        assert_eq!(locator.to_vec(), &txid[..LOCATOR_LEN]);
    }

    #[test]
    fn sign_recover_roundtrip() {
        let sk = SecretKey::from_slice(&[42u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&bitcoin::secp256k1::Secp256k1::new(), &sk);
        let sig = sign(b"lampo watchtower", &sk);
        assert_eq!(recover_pk(b"lampo watchtower", &sig).unwrap(), pk);
    }

    #[test]
    fn wire_json_format() {
        let request = RegisterRequest {
            user_id: vec![2u8; 33],
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["user_id"], "02".repeat(33));

        let request = AddAppointmentRequest {
            appointment: WireAppointment {
                locator: vec![0xab; LOCATOR_LEN],
                encrypted_blob: vec![0xcd; 4],
                to_self_delay: 42,
            },
            signature: "sig".to_owned(),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["appointment"]["locator"], "ab".repeat(LOCATOR_LEN));
        assert_eq!(json["appointment"]["encrypted_blob"], "cd".repeat(4));
        assert_eq!(json["appointment"]["to_self_delay"], 42);
        assert_eq!(json["signature"], "sig");
    }
}
