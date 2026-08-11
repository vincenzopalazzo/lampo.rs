//! Durable state for the watchtower client.
//!
//! Two small file-backed queues under `<datadir>/watchtower/`:
//!
//! - `pending/<channel_id>.json`: justice transaction data captured from
//!   counterparty commitments that are not yet revoked, so they cannot
//!   be signed yet.
//! - `outbox/<dispute_txid>.json`: signed penalty transactions waiting
//!   to be delivered to the tower.
//!
//! Both survive restarts: a commitment revoked while the tower was
//! unreachable is delivered on the next run.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use bitcoin::consensus;
use bitcoin::{Transaction, Txid};

use crate::error;

/// Justice transaction data captured at commitment time, waiting for
/// the counterparty to revoke so it can be signed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingJustice {
    /// Unsigned justice transaction, consensus serialized.
    #[serde(with = "hex::serde")]
    pub justice_tx: Vec<u8>,
    /// Value of the revokeable output being claimed.
    pub value_sat: u64,
    /// Commitment number of the counterparty commitment.
    pub commitment_number: u64,
}

impl PendingJustice {
    pub fn tx(&self) -> error::Result<Transaction> {
        Ok(consensus::deserialize(&self.justice_tx)?)
    }
}

/// A signed penalty transaction ready to be sent to the tower.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedJustice {
    /// The dispute (revoked commitment) txid.
    pub dispute_txid: Txid,
    /// Signed penalty transaction, consensus serialized.
    #[serde(with = "hex::serde")]
    pub penalty_tx: Vec<u8>,
    /// The `to_self_delay` reported in the appointment.
    pub to_self_delay: u32,
}

impl SignedJustice {
    pub fn tx(&self) -> error::Result<Transaction> {
        Ok(consensus::deserialize(&self.penalty_tx)?)
    }
}

/// File-backed watchtower state rooted at `<datadir>/watchtower`.
pub struct Outbox {
    root: PathBuf,
}

impl Outbox {
    pub fn new(root: PathBuf) -> error::Result<Self> {
        fs::create_dir_all(root.join("pending"))?;
        fs::create_dir_all(root.join("outbox"))?;
        Ok(Outbox { root })
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    fn pending_path(&self, channel: &str) -> PathBuf {
        self.root.join("pending").join(format!("{channel}.json"))
    }

    fn outbox_path(&self, dispute_txid: &Txid) -> PathBuf {
        self.root
            .join("outbox")
            .join(format!("{dispute_txid}.json"))
    }

    /// Loads the pending justice queue of a channel (oldest first).
    pub fn load_pending(&self, channel: &str) -> error::Result<Vec<PendingJustice>> {
        let path = self.pending_path(channel);
        if !path.exists() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    /// Stores the pending justice queue of a channel; an empty queue
    /// removes the file.
    pub fn store_pending(&self, channel: &str, queue: &[PendingJustice]) -> error::Result<()> {
        let path = self.pending_path(channel);
        if queue.is_empty() {
            if path.exists() {
                fs::remove_file(path)?;
            }
            return Ok(());
        }
        write_atomic(&path, &serde_json::to_vec(queue)?)
    }

    /// Adds a signed penalty transaction to the delivery queue. Keyed
    /// by dispute txid, so re-captures are idempotent.
    pub fn push_signed(&self, signed: &SignedJustice) -> error::Result<()> {
        write_atomic(
            &self.outbox_path(&signed.dispute_txid),
            &serde_json::to_vec(signed)?,
        )
    }

    /// Lists the signed penalty transactions waiting for delivery.
    pub fn list_signed(&self) -> error::Result<Vec<SignedJustice>> {
        let mut result = Vec::new();
        for entry in fs::read_dir(self.root.join("outbox"))? {
            let entry = entry?;
            match serde_json::from_slice(&fs::read(entry.path())?) {
                Ok(signed) => result.push(signed),
                Err(err) => {
                    log::error!(target: "lampo-watchtower", "skipping corrupted outbox entry {:?}: {err}", entry.path());
                }
            }
        }
        Ok(result)
    }

    /// Removes a delivered appointment from the queue.
    pub fn remove_signed(&self, dispute_txid: &Txid) -> error::Result<()> {
        let path = self.outbox_path(dispute_txid);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

/// Writes a file through a temporary sibling + rename, so a crash never
/// leaves a half-written entry behind.
fn write_atomic(path: &PathBuf, bytes: &[u8]) -> error::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lampo-wt-outbox-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn pending_roundtrip() {
        let outbox = Outbox::new(tmpdir("pending")).unwrap();
        assert!(outbox.load_pending("chan").unwrap().is_empty());

        let queue = vec![PendingJustice {
            justice_tx: vec![1, 2, 3],
            value_sat: 1000,
            commitment_number: 42,
        }];
        outbox.store_pending("chan", &queue).unwrap();
        let loaded = outbox.load_pending("chan").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].value_sat, 1000);
        assert_eq!(loaded[0].commitment_number, 42);

        outbox.store_pending("chan", &[]).unwrap();
        assert!(outbox.load_pending("chan").unwrap().is_empty());
    }

    #[test]
    fn signed_roundtrip() {
        let outbox = Outbox::new(tmpdir("signed")).unwrap();
        let dispute_txid =
            Txid::from_str("d6ac4a5e61657c4c604dcde855a1db74ec6b3e54f32695d72c5e11c7761ea1b4")
                .unwrap();
        let signed = SignedJustice {
            dispute_txid,
            penalty_tx: vec![4, 5, 6],
            to_self_delay: 144,
        };
        outbox.push_signed(&signed).unwrap();
        // Idempotent by dispute txid.
        outbox.push_signed(&signed).unwrap();

        let listed = outbox.list_signed().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].dispute_txid, dispute_txid);

        outbox.remove_signed(&dispute_txid).unwrap();
        assert!(outbox.list_signed().unwrap().is_empty());
    }
}
