//! Wallet Manager implementation with BDK
use std::fmt::Display;
use std::sync::Arc;

use bdk::keys::bip39::{Language, Mnemonic, WordCount};
use bdk::keys::{DerivableKey, ExtendedKey, GeneratableKey, GeneratedKey};
use bdk::template::Bip84;
use bdk::wallet::Balance;
use bdk::{descriptor, FeeRate, KeychainKind, SignOptions, Wallet};
use bdk_chain::ConfirmationTime;
use bdk_esplora::EsploraExt;
use bdk_file_store::KeychainStore;
use bitcoin::util::bip32::ExtendedPrivKey;
use tokio::sync::Mutex;

use lampo_common::bitcoin::{PrivateKey, Script, Transaction};
use lampo_common::conf::{LampoConf, Network};
use lampo_common::model::response::NewAddress;

use crate::async_run;
use crate::keys::keys::LampoKeys;

/// Wallet manager trait that define a generic interface
/// over Wallet implementation!
// FIXME: move this in a lampo_lib
pub trait WalletManager: Send + Sync {
    /// Generate a new wallet for the network
    fn new(conf: Arc<LampoConf>) -> Result<(Self, String), bdk::Error>
    where
        Self: Sized;

    /// Restore a previous created wallet from a network and a mnemonic_words
    fn restore(network: Arc<LampoConf>, mnemonic_words: &str) -> Result<Self, bdk::Error>
    where
        Self: Sized;

    /// Return the keys for ldk.
    fn ldk_keys(&self) -> Arc<LampoKeys>;

    /// return an on chain address
    fn get_onchain_address(&self) -> Result<NewAddress, bdk::Error>;

    /// Get the current balance of the wallet.
    fn get_onchain_balance(&self) -> Result<Balance, bdk::Error>;

    /// Create the transaction from a script and return the transaction
    /// to propagate to the network.
    fn create_transaction(
        &self,
        script: Script,
        amount: u64,
        fee_rate: u32,
    ) -> Result<Transaction, bdk::Error>;

    /// Return the list of transaction stored inside the wallet
    fn list_transactions(&self) -> Result<Vec<Transaction>, bdk::Error>;

    /// Sync the wallet.
    fn sync(&self) -> Result<(), bdk::Error>;
}

pub struct LampoWalletManager {
    // FIXME: remove the mutex here to be sync I used the tokio mutex but this is wrong!
    pub wallet: Mutex<Wallet<KeychainStore<KeychainKind, ConfirmationTime>>>,
    pub keymanager: Arc<LampoKeys>,
    pub network: Network,
}

fn to_bdk_err<T: Display>(err: T) -> bdk::Error {
    bdk::Error::Generic(format!("{err}"))
}

impl LampoWalletManager {
    /// from mnemonic_words build or bkd::Wallet or return an bdk::Error
    fn build_wallet(
        conf: Arc<LampoConf>,
        mnemonic_words: &str,
    ) -> Result<
        (
            Wallet<KeychainStore<KeychainKind, ConfirmationTime>>,
            LampoKeys,
        ),
        bdk::Error,
    > {
        // Parse a mnemonic
        let mnemonic =
            Mnemonic::parse(mnemonic_words).map_err(|err| bdk::Error::Generic(format!("{err}")))?;
        // Generate the extended key
        let xkey: ExtendedKey = mnemonic.into_extended_key()?;
        // Get xprv from the extended key
        let xprv = xkey.into_xprv(conf.network).ok_or(bdk::Error::Generic(
            "wrong convertion to a private key".to_string(),
        ))?;

        let db = KeychainStore::new_from_path(format!("{}/onchain", conf.path()))
            .map_err(|err| bdk::Error::Generic(format!("{err}")))?;
        let ldk_kesy = LampoKeys::new(xprv.private_key.secret_bytes());
        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let wallet = Wallet::new(
            Bip84(xprv, KeychainKind::External),
            Some(Bip84(xprv, KeychainKind::Internal)),
            db,
            conf.network,
        )
        .map_err(|err| bdk::Error::Generic(err.to_string()))?;
        let descriptor = wallet.public_descriptor(KeychainKind::Internal).unwrap();
        log::info!("descriptor: {descriptor}");
        Ok((wallet, ldk_kesy))
    }

    #[cfg(debug_assertions)]
    fn build_from_private_key(
        xprv: PrivateKey,
        channel_keys: Option<String>,
    ) -> Result<
        (
            Wallet<KeychainStore<KeychainKind, ConfirmationTime>>,
            LampoKeys,
        ),
        bdk::Error,
    > {
        let ldk_keys = if channel_keys.is_some() {
            LampoKeys::with_channel_keys(xprv.inner.secret_bytes(), channel_keys.unwrap())
        } else {
            LampoKeys::new(xprv.inner.secret_bytes())
        };

        // FIXME: Get a tmp path
        let db = KeychainStore::new_from_path("/tmp/onchain")
            .map_err(|err| bdk::Error::Generic(format!("{err}")))?;

        let key = ExtendedPrivKey::new_master(xprv.network, &xprv.inner.secret_bytes())?;
        let key = ExtendedKey::from(key);
        let wallet = Wallet::new(Bip84(key, KeychainKind::External), None, db, xprv.network)
            .map_err(|err| bdk::Error::Generic(err.to_string()))?;
        Ok((wallet, ldk_keys))
    }
}

impl WalletManager for LampoWalletManager {
    fn new(conf: Arc<LampoConf>) -> Result<(Self, String), bdk::Error> {
        // Generate fresh mnemonic
        let mnemonic: GeneratedKey<_, bdk::miniscript::Tap> =
            Mnemonic::generate((WordCount::Words12, Language::English))
                .map_err(|err| bdk::Error::Generic(format!("{:?}", err)))?;
        // Convert mnemonic to string
        let mnemonic_words = mnemonic.to_string();
        log::info!("mnemonic works `{mnemonic_words}`");
        let (wallet, keymanager) = LampoWalletManager::build_wallet(conf.clone(), &mnemonic_words)?;
        Ok((
            Self {
                wallet: Mutex::new(wallet),
                keymanager: Arc::new(keymanager),
                network: conf.network,
            },
            mnemonic_words,
        ))
    }

    fn restore(conf: Arc<LampoConf>, mnemonic_words: &str) -> Result<Self, bdk::Error> {
        let (wallet, keymanager) = LampoWalletManager::build_wallet(conf.clone(), mnemonic_words)?;
        Ok(Self {
            wallet: Mutex::new(wallet),
            keymanager: Arc::new(keymanager),
            network: conf.network,
        })
    }

    fn ldk_keys(&self) -> Arc<LampoKeys> {
        self.keymanager.clone()
    }

    fn get_onchain_address(&self) -> Result<NewAddress, bdk::Error> {
        let address = async_run!(self.wallet.lock()).get_address(bdk::wallet::AddressIndex::New);
        Ok(NewAddress {
            address: address.address.to_string(),
        })
    }

    fn get_onchain_balance(&self) -> Result<Balance, bdk::Error> {
        self.sync()?;
        let balance = async_run!(self.wallet.lock()).get_balance();
        Ok(balance)
    }

    fn create_transaction(
        &self,
        script: Script,
        amount: u64,
        fee_rate: u32,
    ) -> Result<Transaction, bdk::Error> {
        self.sync()?;
        let mut wallet = async_run!(self.wallet.lock());
        let mut tx = wallet.build_tx();
        tx.add_recipient(script, amount)
            .fee_rate(FeeRate::from_sat_per_kvb(fee_rate as f32))
            .enable_rbf();
        let (mut psbt, _) = tx.finish()?;
        if !wallet.sign(&mut psbt, SignOptions::default())? {
            return Err(bdk::Error::Generic(format!(
                "wallet not able to sing the psbt {psbt}"
            )));
        }
        if !wallet.finalize_psbt(&mut psbt, SignOptions::default())? {
            return Err(bdk::Error::Generic(format!(
                "wallet impossible finalize the psbt: {}",
                psbt
            )));
        };
        Ok(psbt.extract_tx())
    }

    fn list_transactions(&self) -> Result<Vec<Transaction>, bdk::Error> {
        self.sync()?;
        let wallet = async_run!(self.wallet.lock());
        let txs = wallet.transactions().map(|(_, tx)| tx.to_owned()).collect();
        Ok(txs)
    }

    fn sync(&self) -> Result<(), bdk::Error> {
        // Scanning the chain...
        let esplora_url = match self.network {
            Network::Bitcoin => "https://mempool.space/api",
            Network::Testnet => "https://mempool.space/testnet/api",
            _ => {
                return Err(bdk::Error::Generic(format!(
                    "network `{:?}` not supported",
                    self.network
                )))
            }
        };
        let mut wallet = async_run!(self.wallet.lock());
        let client = bdk_esplora::esplora_client::Builder::new(esplora_url)
            .build_blocking()
            .map_err(to_bdk_err)?;
        let checkpoints = wallet.checkpoints();
        let spks = wallet
            .spks_of_all_keychains()
            .into_iter()
            .map(|(k, spks)| {
                let mut first = true;
                (
                    k,
                    spks.inspect(move |(spk_i, _)| {
                        if first {
                            first = false;
                        }
                    }),
                )
            })
            .collect();
        log::info!("bdk stert to sync");
        let update = client
            .scan(
                checkpoints,
                spks,
                core::iter::empty(),
                core::iter::empty(),
                50,
                2,
            )
            .map_err(to_bdk_err)?;
        wallet.apply_update(update).map_err(to_bdk_err)?;
        wallet.commit().map_err(to_bdk_err)?;
        log::info!(
            "bdk in sync at height {}!",
            client
                .get_height()
                .map_err(|err| bdk::Error::Generic(format!("{err}")))?
        );
        Ok(())
    }
}

impl TryFrom<(PrivateKey, Option<String>)> for LampoWalletManager {
    type Error = bdk::Error;

    fn try_from(value: (PrivateKey, Option<String>)) -> Result<Self, Self::Error> {
        let (wallet, keymanager) = LampoWalletManager::build_from_private_key(value.0, value.1)?;
        Ok(Self {
            wallet: Mutex::new(wallet),
            keymanager: Arc::new(keymanager),
            // This should be possible only during integration testing
            // FIXME: fix the sync method in bdk, the esplora client will crash!
            network: Network::Regtest,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::PrivateKey;

    use lampo_common::secp256k1::SecretKey;

    use super::{LampoWalletManager, WalletManager};

    #[test]
    fn from_private_key() {
        let pkey = PrivateKey::new(
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap(),
            bitcoin::Network::Regtest,
        );
        let wallet = LampoWalletManager::try_from((pkey, None));
        assert!(wallet.is_ok(), "{:?}", wallet.err());
        let wallet = wallet.unwrap();
        assert!(wallet.get_onchain_address().is_ok());
    }
}
