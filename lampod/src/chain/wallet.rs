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
use bitcoin::util::bip32::ExtendedPrivKey;
use tokio::sync::Mutex;

use lampo_common::bitcoin::PrivateKey;
use lampo_common::model::response::NewAddress;

use crate::async_run;
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

    /// return an on chain address
    fn get_onchain_address(&self) -> Result<NewAddress, bdk::Error>;
}

pub struct LampoWalletManager {
    // FIXME: remove the mutex here to be sync I used the tokio mutex but this is wrong!
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

    #[cfg(debug_assertions)]
    fn build_from_private_key(
        xprv: PrivateKey,
        channel_keys: Option<String>,
    ) -> Result<(Wallet<MemoryDatabase>, LampoKeys), bdk::Error> {
        let ldk_keys = if channel_keys.is_some() {
            LampoKeys::with_channel_keys(xprv.inner.secret_bytes(), channel_keys.unwrap())
        } else {
            LampoKeys::new(xprv.inner.secret_bytes())
        };

        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let key = ExtendedPrivKey::new_master(xprv.network, &xprv.inner.secret_bytes())?;
        let key = ExtendedKey::from(key);
        let wallet = Wallet::new(
            Bip84(key, KeychainKind::External),
            None,
            xprv.network,
            MemoryDatabase::default(),
        )?;
        Ok((wallet, ldk_keys))
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

    fn get_onchain_address(&self) -> Result<NewAddress, bdk::Error> {
        let address = async_run!(self.wallet.lock()).get_address(bdk::wallet::AddressIndex::New)?;
        Ok(NewAddress {
            address: address.address.to_string(),
        })
    }
}

impl TryFrom<(PrivateKey, Option<String>)> for LampoWalletManager {
    type Error = bdk::Error;

    fn try_from(value: (PrivateKey, Option<String>)) -> Result<Self, Self::Error> {
        let (wallet, keymanager) = LampoWalletManager::build_from_private_key(value.0, value.1)?;
        Ok(Self {
            wallet: Mutex::new(wallet),
            keymanager: Arc::new(keymanager),
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
