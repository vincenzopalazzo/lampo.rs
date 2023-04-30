//! Wallet Manager implementation with BDK

use std::sync::Arc;

use bdk::database::MemoryDatabase;
use bdk::keys::GeneratableKey;
use bdk::keys::{
    bip39::{Language, Mnemonic, WordCount},
    DerivableKey, ExtendedKey, GeneratedKey,
};
use bdk::miniscript::miniscript;
use bdk::template::Bip84;
use bdk::{KeychainKind, Wallet};
use tokio::sync::Mutex;

use crate::keys::keys::LampoKeys;

/// Wallet manager trait that define a generic interface
/// over Wallet implementation!
// FIXME: move this in a lampo_lib
pub trait WalletManager: Send + Sync {
    /// Generate a new wallet for the network
    fn new(network: super::Network) -> Result<Self, bdk::Error>
    where
        Self: Sized;

    /// Restore a previous created wallet from a network and a mnemonic_words
    fn restore(network: super::Network, mnemonic_words: &str) -> Result<Self, bdk::Error>
    where
        Self: Sized;

    /// Return the keys for ldk.
    fn ldk_keys(&self) -> Arc<LampoKeys>;
}

pub struct LampoWalletManager {
    pub wallet: Mutex<Wallet<MemoryDatabase>>,
    pub keymanager: Arc<LampoKeys>,
}

impl LampoWalletManager {
    /// from mnemonic_words build or bkd::Wallet or return an bdk::Error
    fn build_wallet(
        network: super::Network,
        mnemonic_words: &str,
    ) -> Result<(Wallet<MemoryDatabase>, LampoKeys), bdk::Error> {
        // Parse a mnemonic
        let mnemonic =
            Mnemonic::parse(mnemonic_words).map_err(|err| bdk::Error::Generic(format!("{err}")))?;
        // Generate the extended key
        let xkey: ExtendedKey = mnemonic.into_extended_key()?;
        // Get xprv from the extended key
        let xprv = xkey.into_xprv(network).ok_or(bdk::Error::Generic(
            "wrong convertion to a private key".to_string(),
        ))?;

        let ldk_kesy = LampoKeys::new(xprv.private_key.secret_bytes());
        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let wallet = Wallet::new(
            Bip84(xprv, KeychainKind::External),
            Some(Bip84(xprv, KeychainKind::Internal)),
            network,
            MemoryDatabase::default(),
        )?;
        Ok((wallet, ldk_kesy))
    }
}

impl WalletManager for LampoWalletManager {
    fn new(network: super::Network) -> Result<Self, bdk::Error> {
        // Generate fresh mnemonic
        let mnemonic: GeneratedKey<_, miniscript::Segwitv0> =
            Mnemonic::generate((WordCount::Words12, Language::English))
                .map_err(|err| bdk::Error::Generic(format!("{:?}", err)))?;
        // Convert mnemonic to string
        let mnemonic_words = mnemonic.to_string();
        // FIXME store the mnemonic somewhere to allow to restore it
        let (wallet, keymanager) = LampoWalletManager::build_wallet(network, &mnemonic_words)?;
        Ok(Self {
            wallet: Mutex::new(wallet),
            keymanager: Arc::new(keymanager),
        })
    }

    fn restore(network: super::Network, mnemonic_words: &str) -> Result<Self, bdk::Error> {
        let (wallet, keymanager) = LampoWalletManager::build_wallet(network, mnemonic_words)?;
        Ok(Self {
            wallet: Mutex::new(wallet),
            keymanager: Arc::new(keymanager),
        })
    }

    fn ldk_keys(&self) -> Arc<LampoKeys> {
        self.keymanager.clone()
    }
}
