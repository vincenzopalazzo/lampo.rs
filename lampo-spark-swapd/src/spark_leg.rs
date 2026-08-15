//! The Spark leg: a thin wrapper over `SparkWallet`. Only payment
//! hashes, amounts, addresses and expiries cross this boundary — the
//! engine never sees Spark protocol types.
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bitcoin::hashes::sha256;

use lampo_common::error;
use spark::address::SparkAddress;
use spark::services::Preimage;
use spark_wallet::{ListTransfersRequest, SparkWallet, TransferId};

/// An incoming HTLC waiting on a preimage from us.
#[derive(Debug, Clone)]
pub struct ClaimableHtlc {
    pub payment_hash: String,
    pub amount_sat: u64,
    /// When the lock releases back to the sender. The engine must not
    /// pay the other leg unless this leaves room to claim afterwards.
    pub expiry: SystemTime,
}

pub struct SparkLeg {
    wallet: Arc<SparkWallet>,
}

impl SparkLeg {
    pub fn new(wallet: Arc<SparkWallet>) -> Self {
        Self { wallet }
    }

    /// Lock a Spark HTLC to `receiver` on `payment_hash_hex`.
    ///
    /// `transfer_id` is the idempotency key and is not optional by
    /// accident: the caller persists it *before* calling, so a retry
    /// after a crash reuses the same id and Spark refuses to create a
    /// second transfer instead of paying twice.
    pub async fn create_htlc(
        &self,
        amount_sat: u64,
        receiver: &str,
        payment_hash_hex: &str,
        expiry: Duration,
        transfer_id: &str,
    ) -> error::Result<String> {
        let receiver = SparkAddress::from_str(receiver)
            .map_err(|err| error::anyhow!("invalid spark address: {err}"))?;
        let hash = sha256::Hash::from_str(payment_hash_hex)
            .map_err(|err| error::anyhow!("invalid payment hash: {err}"))?;
        let transfer_id = TransferId::from_str(transfer_id)
            .map_err(|err| error::anyhow!("invalid transfer id: {err}"))?;
        let transfer = self
            .wallet
            .create_htlc(amount_sat, &receiver, &hash, expiry, Some(transfer_id))
            .await
            .map_err(|err| error::anyhow!("spark create_htlc: {err}"))?;
        Ok(transfer.id.to_string())
    }

    /// Claim an incoming Spark HTLC with the preimage learned from the
    /// lightning leg (Direction A settlement).
    pub async fn claim_htlc(&self, preimage_hex: &str) -> error::Result<()> {
        let preimage = Preimage::from_hex(preimage_hex)
            .map_err(|err| error::anyhow!("invalid preimage: {err}"))?;
        self.wallet
            .claim_htlc(&preimage)
            .await
            .map_err(|err| error::anyhow!("spark claim_htlc: {err}"))?;
        Ok(())
    }

    /// Every incoming HTLC currently waiting on a preimage from us.
    /// Amount and expiry are carried because a counterparty chooses
    /// both: they can lock any amount, for any duration, against a hash
    /// we quoted, and the engine has to check both before it pays.
    pub async fn claimable_htlcs(&self) -> error::Result<Vec<ClaimableHtlc>> {
        let transfers = self
            .wallet
            .list_claimable_htlc_transfers(None)
            .await
            .map_err(|err| error::anyhow!("spark list_claimable_htlc_transfers: {err}"))?;
        Ok(transfers
            .into_iter()
            .filter_map(|transfer| {
                let amount_sat = transfer.total_value_sat;
                transfer.htlc_preimage_request.map(|request| ClaimableHtlc {
                    payment_hash: request.payment_hash.to_string(),
                    amount_sat,
                    expiry: request.expiry_time,
                })
            })
            .collect())
    }

    /// The preimage of an HTLC *we* locked, once the receiver has
    /// claimed it and revealed the secret. This is how the atomic
    /// direction learns the value that settles its lightning leg.
    pub async fn revealed_preimage(&self, transfer_id: &str) -> error::Result<Option<String>> {
        let id = TransferId::from_str(transfer_id)
            .map_err(|err| error::anyhow!("invalid transfer id: {err}"))?;
        let transfers = self
            .wallet
            .list_transfers(ListTransfersRequest {
                paging: None,
                transfer_ids: vec![id],
            })
            .await
            .map_err(|err| error::anyhow!("spark list_transfers: {err}"))?;
        Ok(transfers.items.into_iter().find_map(|transfer| {
            transfer
                .htlc_preimage_request
                .and_then(|request| request.preimage)
                .map(|preimage| hex_of(&preimage.to_vec()))
        }))
    }

    /// Does a transfer with this id exist?
    ///
    /// A failed `create_htlc` is ambiguous: the transfer may never have
    /// been created, or it may have been created and the response lost.
    /// Treating the second case as "not delivered" is a funds-loss bug —
    /// the counterparty holds a live, claimable HTLC while we believe we
    /// owe them one. Because the transfer id is chosen before the call,
    /// we can settle the question by asking.
    pub async fn transfer_exists(&self, transfer_id: &str) -> error::Result<bool> {
        let id = TransferId::from_str(transfer_id)
            .map_err(|err| error::anyhow!("invalid transfer id: {err}"))?;
        let transfers = self
            .wallet
            .list_transfers(ListTransfersRequest {
                paging: None,
                transfer_ids: vec![id],
            })
            .await
            .map_err(|err| error::anyhow!("spark list_transfers: {err}"))?;
        Ok(!transfers.items.is_empty())
    }

    /// Re-shape the wallet's leaves so an arbitrary amount can be sent.
    ///
    /// A deposit lands as one leaf, and `create_htlc` cannot mint change
    /// from a single leaf, so a partial-amount payout fails with
    /// `InsufficientFunds`. Running the optimizer splits and merges
    /// leaves into spendable denominations first. Best effort: if it
    /// makes no progress the subsequent `create_htlc` will surface the
    /// real error.
    pub async fn optimize(&self, max_rounds: u32) -> error::Result<()> {
        self.wallet
            .optimize_leaves(Some(max_rounds))
            .await
            .map_err(|err| error::anyhow!("spark optimize_leaves: {err}"))?;
        Ok(())
    }

    /// The wallet's spendable balance in sats. Used to refuse a
    /// Direction B swap up front when we could not deliver it, rather
    /// than making the counterparty pay into a hold we cannot fulfil.
    pub async fn balance(&self) -> error::Result<u64> {
        // Sync first. Leaves claimed outside this process -- an operator
        // topping the daemon up with a deposit is the normal case -- are
        // invisible to a cached balance, so without this the daemon
        // reports 0 forever and the deliverability fence refuses every
        // receive swap with no way to tell why.
        if let Err(err) = self.wallet.sync().await {
            log::warn!(target: "swapd", "spark sync before balance failed: {err}");
        }
        self.wallet
            .get_balance()
            .await
            .map_err(|err| error::anyhow!("spark get_balance: {err}"))
    }

    pub async fn spark_address(&self) -> error::Result<String> {
        let address = self
            .wallet
            .get_spark_address()
            .map_err(|err| error::anyhow!("spark address: {err}"))?;
        address
            .to_address_string()
            .map_err(|err| error::anyhow!("spark address encoding: {err}"))
    }
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
