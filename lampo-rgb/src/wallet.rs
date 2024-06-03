use std::{
    cell::RefCell,
    fs,
    path::{self, Path},
    sync::{Arc, Mutex},
};

use bdk::bitcoin::bip32::{ChildNumber, ExtendedPrivKey};
use bdk::keys::bip39::{Language, Mnemonic};
use lampo_common::{
    conf::{LampoConf, Network},
    error,
    keys::LampoKeys,
    model::response::{self, Utxo},
    secp256k1::Secp256k1,
    wallet::WalletManager,
};
use rgb_lib::{
    wallet::{AssetCFA, Online, WalletData},
    BitcoinNetwork, Wallet,
};

pub struct LampoRgbWallet {
    keys_manager: Arc<LampoKeys>,
    // conf : RefCell<Mutex<LampoConf>>,
    // This should be the main wallet here.
    offline: RefCell<Mutex<Wallet>>,
    online: RefCell<Mutex<Option<Online>>>,
    #[allow(dead_code)]
    proxy: String,
    #[allow(dead_code)]
    url: String,
}

unsafe impl Send for LampoRgbWallet {}
unsafe impl Sync for LampoRgbWallet {}

// FIXME: Move this function somwhere else.
pub(crate) fn get_coin_type(bitcoin_network: BitcoinNetwork) -> u32 {
    u32::from(bitcoin_network != BitcoinNetwork::Mainnet)
}

// Idk why this isn't public in rgb-lib?
pub(crate) fn derive_account_xprv_from_mnemonic(
    bitcoin_network: BitcoinNetwork,
    mnemonic: &str,
) -> lampo_common::error::Result<ExtendedPrivKey> {
    let coin_type = get_coin_type(bitcoin_network);
    let account_derivation_path = vec![
        ChildNumber::from_hardened_idx(PURPOSE as u32).unwrap(),
        ChildNumber::from_hardened_idx(coin_type).unwrap(),
        ChildNumber::from_hardened_idx(ACCOUNT as u32).unwrap(),
    ];
    let mnemonic = Mnemonic::parse_in(Language::English, mnemonic.to_string())?;
    let master_xprv =
        ExtendedPrivKey::new_master(bitcoin_network.into(), &mnemonic.to_seed("")).unwrap();
    Ok(master_xprv.derive_priv(&Secp256k1::new(), &account_derivation_path)?)
}

// TODO: Handle error and where should we put this to?
pub fn get_urls(conf: Arc<LampoConf>) -> (String, String) {
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
    (url.unwrap().to_string(), proxy.unwrap().to_string())
}

pub(crate) const PURPOSE: u8 = 84;
pub(crate) const ACCOUNT: u8 = 0;

impl LampoRgbWallet {
    fn rgb_wallet_exists(conf: Arc<LampoConf>) -> bool {
        let data_dir = format!("{}/{}/rgb/mnemonic_path", conf.root_path, conf.network);
        let rgb_wallet_path = Path::new(&data_dir);
        let exists = path::Path::exists(rgb_wallet_path);
        exists
    }

    #[allow(dead_code)]
    pub(crate) fn rgb_issue_asset_cfa(
        &self,
        name: String,
        details: Option<String>,
        precision: u8,
        amounts: Vec<u64>,
        file_path: Option<String>,
    ) -> AssetCFA {
        let issue = self
            .offline
            .borrow()
            .lock()
            .unwrap()
            .issue_asset_cfa(
                self.online
                    .borrow()
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .clone(),
                name,
                details,
                precision,
                amounts,
                file_path,
            )
            .unwrap();
        issue
    }
}

impl WalletManager for LampoRgbWallet {
    fn new(conf: Arc<LampoConf>) -> lampo_common::error::Result<(Self, String)>
    where
        Self: Sized,
    {
        // Mostly Taken from easy-rgb and our core-wallet implementation.
        // data_dir = .lampo/testnet/rgb/
        // root_path = /home/doom/.lampo
        let data_dir = format!("{}/{}/rgb/", conf.root_path, conf.network);
        let _ = std::fs::create_dir(data_dir.clone());
        println!("Created dir successfully!");
        let network = match &conf.network {
            Network::Testnet => BitcoinNetwork::Testnet,
            Network::Regtest => BitcoinNetwork::Regtest,
            Network::Signet => BitcoinNetwork::Signet,
            Network::Bitcoin => BitcoinNetwork::Mainnet,
            _ => panic!("Wrong network!"),
        };
        let res = rgb_lib::keys::generate_keys(network);
        let xpub = res.account_xpub;
        let mnemonic = res.mnemonic;
        let xpriv = derive_account_xprv_from_mnemonic(conf.network.into(), mnemonic.as_str());

        // Saving the mnemonic to the path system. FIXME: This should be encrypted.
        let mnemonic_path = format!("{}/mnemonic_path", data_dir);
        let _ = std::fs::File::create(mnemonic_path.clone());
        // Handle the error.
        let res = fs::write(mnemonic_path, mnemonic.clone());

        // Storing this key in keys manager
        let ldk_keys = LampoKeys::new(xpriv.unwrap().private_key.secret_bytes());

        let wallet_data = WalletData {
            data_dir,
            bitcoin_network: network.into(),
            database_type: rgb_lib::wallet::DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            pubkey: xpub.to_string(),
            mnemonic: Some(mnemonic.to_string()),
            vanilla_keychain: None,
        };

        // Extended Key
        let (url, proxy) = get_urls(conf);
        let mut wallet = Wallet::new(wallet_data)?;
        // Handle this error.
        println!("Go online started!");
        let online = wallet.go_online(false, url.clone()).unwrap();
        println!("Online : {:?}", online);
        println!("Go online success!");
        let lampo_wallet = LampoRgbWallet {
            keys_manager: Arc::new(ldk_keys),
            offline: RefCell::new(Mutex::new(wallet)),
            online: RefCell::new(Mutex::new(Some(online))),
            url: url.to_owned(),
            proxy: proxy.to_owned(),
        };
        println!("Wallet created successfully!");
        Ok((lampo_wallet, mnemonic.to_string()))
    }

    // This has many bugs.
    fn restore(network: Arc<LampoConf>, mnemonic_words: &str) -> lampo_common::error::Result<Self>
    where
        Self: Sized,
    {
        let wallet_exists = Self::rgb_wallet_exists(network.clone());
        if wallet_exists {
            let data_dir = format!("{}/{}/rgb/", network.root_path, network.network);
            let mnemonic_file = format!("{}mnemonic_path", data_dir);
            let mnemoic_path = fs::read_to_string(mnemonic_file).expect("Error reading");
            if mnemoic_path.to_string() != mnemonic_words {
                Err(error::anyhow!("Wrong mnemonic"))
            } else {
                let keys =
                    rgb_lib::keys::restore_keys(network.network.into(), mnemonic_words.to_string())
                        .unwrap();
                let wallet_data = WalletData {
                    data_dir,
                    bitcoin_network: network.network.into(),
                    database_type: rgb_lib::wallet::DatabaseType::Sqlite,
                    max_allocations_per_utxo: 1,
                    pubkey: keys.account_xpub,
                    mnemonic: Some(mnemonic_words.to_string()),
                    vanilla_keychain: None,
                };
                let mut rgb = Wallet::new(wallet_data).unwrap();
                let xpriv =
                    derive_account_xprv_from_mnemonic(network.network.into(), mnemonic_words);
                let ldk_keys = LampoKeys::new(xpriv.unwrap().private_key.secret_bytes());
                let (url, proxy) = get_urls(network);
                let online = rgb.go_online(false, url.clone());
                Ok(LampoRgbWallet {
                    keys_manager: Arc::new(ldk_keys),
                    offline: RefCell::new(Mutex::new(rgb)),
                    online: RefCell::new(Mutex::new(Some(online.unwrap()))),
                    proxy,
                    url,
                })
            }
        } else {
            Err(error::anyhow!("No wallet"))
        }
    }

    fn ldk_keys(&self) -> Arc<lampo_common::keys::LampoKeys> {
        self.keys_manager.clone()
    }

    fn get_onchain_address(
        &self,
    ) -> lampo_common::error::Result<lampo_common::model::response::NewAddress> {
        let address = self.offline.borrow().lock().unwrap().get_address()?;
        let net_addr = response::NewAddress { address };
        Ok(net_addr)
    }

    fn get_onchain_balance(&self) -> lampo_common::error::Result<u64> {
        // There are two types of btc balance available. 1st being the normal vanilla one and
        // second being the colored one.

        let online = self
            .online
            .borrow()
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .clone();
        let btc_balance = self
            .offline
            .borrow()
            .lock()
            .unwrap()
            .get_btc_balance(online.clone())
            .unwrap();
        // TODO: See what we have to do with all this.
        let colored_bal_settled = btc_balance.colored.settled;
        let colored_bal_spendable = btc_balance.colored.spendable;
        let vanilla_bal_spendable = btc_balance.vanilla.spendable;
        let vanilla_bal_settled = btc_balance.vanilla.settled;
        Ok(colored_bal_settled
            + colored_bal_spendable
            + vanilla_bal_settled
            + vanilla_bal_spendable)
    }

    fn create_transaction(
        &self,
        script: lampo_common::bitcoin::ScriptBuf,
        amount_sat: u64,
        fee_rate: u32,
    ) -> lampo_common::error::Result<lampo_common::bitcoin::Transaction> {
        // We first need to color all the UTXOs.
        todo!()
    }

    fn list_transactions(
        &self,
    ) -> lampo_common::error::Result<Vec<lampo_common::model::response::Utxo>> {
        let online = self
            .online
            .borrow()
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .clone();
        let txs_result = self
            .offline
            .borrow()
            .lock()
            .unwrap()
            .list_transactions(Some(online));

        // We need to find a way to deal with this as txs_result have txs field different from UTXOs.
        let temp: Vec<Utxo> = todo!();
        Ok(temp)
    }

    fn sync(&self) -> lampo_common::error::Result<()> {
        Ok(())
    }
}
