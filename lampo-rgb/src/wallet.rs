use std::{borrow::Borrow, cell::RefCell, sync::{Arc, Mutex}};

use bdk::{blockchain::ElectrumBlockchain, electrum_client::Client, keys::{bip39::{self, Language, Mnemonic, WordCount}, GeneratableKey, GeneratedKey}, miniscript::Tap};
use lampo_common::{bitcoin::error, conf::{LampoConf, Network}, keys::LampoKeys, model::{request::NewAddress, response::{self, Utxo}}, secp256k1::Secp256k1, wallet::WalletManager};
use rgb_lib::{wallet::{Online, WalletData}, BitcoinNetwork, Wallet};
use bdk::keys::DerivableKey;
use bdk::keys::ExtendedKey;

pub struct LampoRgbWallet {
    keys_manager: Arc<LampoKeys>,
    // conf : RefCell<Mutex<LampoConf>>,
    // This should be the main wallet here.
    offline: RefCell<Mutex<Wallet>>,
    online: RefCell<Mutex<Option<Online>>>,
    proxy: String,
    url: String,
}

unsafe impl Send for LampoRgbWallet { }
unsafe impl Sync for LampoRgbWallet { }

// TODO: Implement the default implementation.
impl LampoRgbWallet { }

impl WalletManager for LampoRgbWallet {
    fn new(conf: Arc<LampoConf>) -> lampo_common::error::Result<(Self, String)>
    where
        Self: Sized {
        // Mostyl Taken from easy-rgb and our core-wallet implementation.
        // data_dir = .lampo/testnet/rgb/
        // root_path = /home/doom/.lampo
        let data_dir = format!("{}/{}/rgb/", conf.root_path, conf.network);
        let network = match &conf.network {
            Network::Testnet => BitcoinNetwork::Testnet,
            Network::Regtest => BitcoinNetwork::Regtest,
            Network::Signet => BitcoinNetwork::Signet,
            Network::Bitcoin => BitcoinNetwork::Mainnet,
            _ => panic!("Wrong network!")
        };
        // Return error from here
        let mnemonic: GeneratedKey<_, bdk::miniscript::Tap> = bip39::Mnemonic::generate((WordCount::Words12, Language::English)).unwrap();
        // Extended Key
        let mnemonic = Mnemonic::parse(&mnemonic.to_string()).unwrap();
        let xkey: ExtendedKey = mnemonic.clone().into_extended_key()?;
        let (url, proxy) = match conf.network {
            Network::Bitcoin => (None, None),
            Network::Testnet => (
                Some("ssl://electrum.iriswallet.com:50013"),
                Some("rpcs://proxy.iriswallet.com/0.2/json-rpc"),
            ),
            Network::Regtest => (
                Some("127.0.0.1:50001"),
                Some("rpc://127.0.0.1:3000/json-rpc"),
            ),
            Network::Signet => (None, None),
            _ => panic!("Wrong network"),
        };

        let (Some(url), Some(proxy)) = (url, proxy) else {
            panic!("Network `{network}` not supported by the plugin");
        };

        // Get xprv from the extended key. 
        // TODO: Handle error
        let xprv = xkey
            .into_xprv(conf.network).unwrap();
        // Get xpub from the extended key
        let xpub = rgb_lib::utils::get_account_xpub(network, &mnemonic.to_string()).unwrap();

        let ldk_keys = LampoKeys::new(xprv.private_key.secret_bytes());
        let wallet_data = WalletData {
            data_dir,
            bitcoin_network: network,
            database_type: rgb_lib::wallet::DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            pubkey: xpub.to_string(),
            mnemonic: Some(mnemonic.to_string()),
            vanilla_keychain: None,
        };

        let wallet = Wallet::new(wallet_data)?;        
        let lampo_wallet = LampoRgbWallet {
            keys_manager: Arc::new(ldk_keys),
            offline: RefCell::new(Mutex::new(wallet)),
            online: RefCell::new(Mutex::new(None)),
            url: url.to_owned(),
            proxy: proxy.to_owned(),
        };
        Ok((lampo_wallet, mnemonic.to_string()))
    }

    fn restore(network: Arc<LampoConf>, mnemonic_words: &str) -> lampo_common::error::Result<Self>
    where
        Self: Sized {
        // Derive xpriv from network and mnemonic_words then, try to restore.
        todo!()
    }

    fn ldk_keys(&self) -> Arc<lampo_common::keys::LampoKeys> {
        self.keys_manager.clone()
    }

    fn get_onchain_address(&self) -> lampo_common::error::Result<lampo_common::model::response::NewAddress> {
        let address = self.offline.borrow().lock().unwrap().get_address()?;
        let net_addr = response::NewAddress {address};
        Ok(net_addr)
    }

    fn get_onchain_balance(&self) -> lampo_common::error::Result<u64> {
        // There are two types of btc balance available. 1st being the normal vanilla one and 
        // second being the colored one.

        //TODO: Check if the online is none of not.
        // At this point we are sure that the online is not `None`.
        let online = self.online.borrow().lock().unwrap().as_mut().unwrap().clone();
        let btc_balance = self.offline.borrow().lock().unwrap().get_btc_balance(online.clone()).unwrap();
        // TODO: See what we have to do with all this.
        let colored_bal_settled = btc_balance.colored.settled;
        let colored_bal_spendable = btc_balance.colored.spendable;
        let vanilla_bal_spendable = btc_balance.vanilla.spendable;
        let vanilla_bal_settled = btc_balance.vanilla.settled;
        Ok(colored_bal_settled + colored_bal_spendable + vanilla_bal_settled + vanilla_bal_spendable)
    }

    fn create_transaction(
        &self,
        script: lampo_common::bitcoin::ScriptBuf,
        amount_sat: u64,
        fee_rate: u32,
    ) -> lampo_common::error::Result<lampo_common::bitcoin::Transaction> {
        todo!()
    }

    fn list_transactions(&self) -> lampo_common::error::Result<Vec<lampo_common::model::response::Utxo>> {
        let online = self.online.borrow().lock().unwrap().as_mut().unwrap().clone();
        let txs_result = self.offline.borrow().lock().unwrap().list_transactions(Some(online));

        // We need to find a way to deal with this as txs_result have txs field different from UTXOs.
        let temp: Vec<Utxo> = todo!();
        Ok(temp)
    }

    // inside bdk.rs
    fn sync(&self) -> lampo_common::error::Result<()> {
        todo!()
    }
}