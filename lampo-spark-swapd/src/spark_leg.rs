//! The Spark leg: a thin wrapper over `SparkWallet`. Only payment
//! hashes, amounts and addresses cross this boundary — the engine
//! never sees Spark protocol types.
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::hashes::sha256;

use lampo_common::error;
use spark::address::SparkAddress;
use spark::services::Preimage;
use spark_wallet::SparkWallet;

pub struct SparkLeg {
    wallet: Arc<SparkWallet>,
}

impl SparkLeg {
    pub fn new(wallet: Arc<SparkWallet>) -> Self {
        Self { wallet }
    }

    /// Lock a Spark HTLC to `receiver` on `payment_hash_hex`
    /// (Direction B: we pay the counterparty once the LN leg settled).
    pub async fn create_htlc(
        &self,
        amount_sat: u64,
        receiver: &str,
        payment_hash_hex: &str,
        expiry: Duration,
    ) -> error::Result<String> {
        let receiver = SparkAddress::from_str(receiver)
            .map_err(|err| error::anyhow!("invalid spark address: {err}"))?;
        let hash = sha256::Hash::from_str(payment_hash_hex)
            .map_err(|err| error::anyhow!("invalid payment hash: {err}"))?;
        let transfer = self
            .wallet
            .create_htlc(amount_sat, &receiver, &hash, expiry, None)
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

    /// Payment hashes of every incoming HTLC currently waiting on a
    /// preimage from us. Direction A polls this to learn the
    /// counterparty locked their leg.
    pub async fn claimable_payment_hashes(&self) -> error::Result<Vec<String>> {
        let transfers = self
            .wallet
            .list_claimable_htlc_transfers(None)
            .await
            .map_err(|err| error::anyhow!("spark list_claimable_htlc_transfers: {err}"))?;
        Ok(transfers
            .into_iter()
            .filter_map(|transfer| {
                transfer
                    .htlc_preimage_request
                    .map(|request| request.payment_hash.to_string())
            })
            .collect())
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
