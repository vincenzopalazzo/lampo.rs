//! BOLT 12 payer proof material.
//!
//! LDK hands us the paid invoice on `Event::PaymentSent` and nowhere else, so we
//! store it together with the preimage. Disclosure is committed to when the proof
//! is signed, which means a proof cannot be widened after the fact: rebuilding it
//! with more disclosed fields requires the original invoice.
//!
//! Nothing else needs storing. The payer signing key is re-derived from the
//! invoice's own payer metadata, and the `ExpandedKey` behind that derivation
//! comes from the node seed.
use std::sync::Arc;

use lampo_common::error;
use lampo_common::ldk::events::PaidBolt12Invoice;
use lampo_common::ldk::io::{Cursor, ErrorKind};
use lampo_common::ldk::ln::channelmanager::PaymentId;
use lampo_common::ldk::ln::inbound_payment::ExpandedKey;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::ldk::util::ser::{Readable, Writeable};
use lampo_common::secp256k1::Secp256k1;

use crate::persistence::LampoPersistence;

/// Namespace holding the payer proof material, keyed by hex payment id.
///
/// FIXME: records are never pruned, so a node accumulates one invoice per BOLT 12
/// payment forever. Needs a retention policy, or an RPC to drop a record once the
/// payer no longer cares about proving that payment.
pub const PAYER_PROOF_NAMESPACE: &str = "payer_proofs";

/// Layout version of a stored record, so the format can change later without
/// misreading old entries.
const RECORD_VERSION: u8 = 1;

/// Byte offsets in an encoded record: version, then preimage, then the flag
/// saying whether an invoice follows.
const PREIMAGE_START: usize = 1;
const PREIMAGE_END: usize = PREIMAGE_START + 32;
const INVOICE_FLAG: usize = PREIMAGE_END;
const HEADER_LEN: usize = INVOICE_FLAG + 1;

/// What is needed to (re)build a payer proof for a settled payment.
pub struct PayerProofRecord {
    pub preimage: PaymentPreimage,
    /// `None` for BOLT 11 payments, which have no payer proof.
    pub invoice: Option<PaidBolt12Invoice>,
}

impl PayerProofRecord {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(RECORD_VERSION);
        buf.extend_from_slice(&self.preimage.0);
        match &self.invoice {
            Some(invoice) => {
                buf.push(1);
                buf.extend_from_slice(&invoice.encode());
            }
            None => buf.push(0),
        }
        buf
    }

    fn decode(buf: &[u8]) -> error::Result<Self> {
        if buf.len() < HEADER_LEN {
            error::bail!("payer proof record too short: {} bytes", buf.len());
        }
        if buf[0] != RECORD_VERSION {
            error::bail!("unsupported payer proof record version {}", buf[0]);
        }

        let mut preimage = [0u8; 32];
        preimage.copy_from_slice(&buf[PREIMAGE_START..PREIMAGE_END]);

        let invoice = match buf[INVOICE_FLAG] {
            0 => None,
            1 => {
                let mut cursor = Cursor::new(&buf[HEADER_LEN..]);
                Some(
                    <PaidBolt12Invoice as Readable>::read(&mut cursor)
                        .map_err(|err| error::anyhow!("decoding paid invoice: {:?}", err))?,
                )
            }
            flag => error::bail!("unknown payer proof invoice flag {flag}"),
        };

        Ok(Self {
            preimage: PaymentPreimage(preimage),
            invoice,
        })
    }
}

/// Records are keyed by payment hash, not payment id.
///
/// `pay_offer` derives the payment id from the offer string alone, so every
/// payment of a reusable offer shares one id and would overwrite the previous
/// record. The payment hash is unique per payment, and it is also what a caller
/// already holds from the `pay` response.
fn key(payment_hash: &PaymentHash) -> String {
    lampo_common::hex::encode(payment_hash.0)
}

/// Persist the material for `payment_hash`, overwriting any earlier record.
///
/// This takes the concrete store on purpose. Lampo has one backend today, and
/// designing a persistence interface around a single implementation is how you
/// get the wrong interface; that work is happening separately, against a real
/// second backend.
pub fn store(
    persister: &Arc<LampoPersistence>,
    payment_hash: &PaymentHash,
    record: &PayerProofRecord,
) -> error::Result<()> {
    persister.write(
        PAYER_PROOF_NAMESPACE,
        "",
        &key(payment_hash),
        record.encode(),
    )?;
    Ok(())
}

/// Load the material for `payment_hash`, if any was stored.
pub fn load(
    persister: &Arc<LampoPersistence>,
    payment_hash: &PaymentHash,
) -> error::Result<Option<PayerProofRecord>> {
    match persister.read(PAYER_PROOF_NAMESPACE, "", &key(payment_hash)) {
        Ok(buf) => Ok(Some(PayerProofRecord::decode(&buf)?)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Build a bech32 payer proof disclosing only the fields LDK includes by
/// default. Widening the disclosure is a separate, explicit decision: a proof
/// cannot be narrowed once handed out.
///
/// Returns `None` when the payment cannot produce one (BOLT 11, or an async
/// payment settled against a static invoice) — that is not an error, the payment
/// itself still succeeded.
pub fn build(
    record: &PayerProofRecord,
    expanded_key: &ExpandedKey,
    payment_id: PaymentId,
) -> Option<String> {
    let invoice = record.invoice.as_ref()?;
    let secp_ctx = Secp256k1::new();

    let builder = invoice
        .prove_payer_derived(record.preimage, expanded_key, payment_id, &secp_ctx)
        .map_err(|err| {
            log::warn!(target: "lampo::payer_proof", "cannot build payer proof: {err:?}");
        })
        .ok()?;

    builder
        .build_and_sign()
        .map(|proof| proof.to_string())
        .map_err(|err| {
            log::warn!(target: "lampo::payer_proof", "cannot sign payer proof: {err:?}");
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_without_invoice_roundtrips() {
        let record = PayerProofRecord {
            preimage: PaymentPreimage([7u8; 32]),
            invoice: None,
        };
        let decoded = PayerProofRecord::decode(&record.encode()).unwrap();
        assert_eq!(decoded.preimage.0, [7u8; 32]);
        assert!(decoded.invoice.is_none());
    }

    #[test]
    fn decode_rejects_truncated_record() {
        assert!(PayerProofRecord::decode(&[RECORD_VERSION, 0, 0]).is_err());
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let mut buf = PayerProofRecord {
            preimage: PaymentPreimage([1u8; 32]),
            invoice: None,
        }
        .encode();
        buf[0] = RECORD_VERSION + 1;
        assert!(PayerProofRecord::decode(&buf).is_err());
    }
}
