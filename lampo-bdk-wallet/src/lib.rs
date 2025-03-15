//! Wallet Manager implementation with BDK
use std::cell::RefCell;
use std::sync::mpsc::{channel, sync_channel};
use std::sync::{Arc, Mutex};

use bdk_bitcoind_rpc::bitcoincore_rpc::{Auth, Client};
use bdk_bitcoind_rpc::Emitter;
use bdk_wallet::bitcoin::{Amount, FeeRate};
use bdk_wallet::file_store::Store;
use bdk_wallet::keys::bip39::{Language, Mnemonic, WordCount};
use bdk_wallet::keys::{DerivableKey, ExtendedKey, GeneratableKey, GeneratedKey};
use bdk_wallet::template::Bip84;
use bdk_wallet::{ChangeSet, KeychainKind, PersistedWallet, SignOptions, Wallet};

use lampo_common::bitcoin::{Block, PrivateKey, ScriptBuf, Transaction};
use lampo_common::conf::{LampoConf, Network};
use lampo_common::keys::LampoKeys;
use lampo_common::model::response::{NewAddress, Utxo};
use lampo_common::wallet::{self, WalletManager};
use lampo_common::{async_trait, error};

pub struct BDKWalletManager {
    pub wallet: RefCell<Mutex<PersistedWallet<Store<ChangeSet>>>>,
    pub rpc: Arc<Client>,
    pub keymanager: Arc<LampoKeys>,
    pub network: Network,
}

// SAFETY: It is safe to do because the `LampoWalletManager`
// is not send and sync due the RefCell, but we use the Mutex
// inside, so we are safe to share across threads.
unsafe impl Send for BDKWalletManager {}
unsafe impl Sync for BDKWalletManager {}

impl BDKWalletManager {
    /// from mnemonic_words build or bkd::Wallet or return an bdk::Error
    async fn build_wallet(
        conf: Arc<LampoConf>,
        mnemonic_words: &str,
    ) -> error::Result<(PersistedWallet<Store<ChangeSet>>, LampoKeys)> {
        // Parse a mnemonic
        let mnemonic = Mnemonic::parse(mnemonic_words)?;
        // Generate the extended key
        let xkey: ExtendedKey = mnemonic.into_extended_key()?;
        // Get xprv from the extended key
        let xprv = xkey
            .into_xprv(conf.network)
            .ok_or(error::anyhow!("Error converting xpriv"))?;

        let mut db = Store::<ChangeSet>::open_or_create_new(
            "lampo".as_bytes(),
            format!("{}/bdk-wallet.db", conf.path()),
        )?;
        let ldk_keys = LampoKeys::new(xprv.private_key.secret_bytes());
        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let wallet = Wallet::load()
            .descriptor(
                bdk_wallet::KeychainKind::External,
                Some(Bip84(xprv, KeychainKind::External)),
            )
            .descriptor(
                bdk_wallet::KeychainKind::Internal,
                Some(Bip84(xprv, KeychainKind::Internal)),
            )
            .extract_keys()
            .check_network(conf.network)
            .load_wallet(&mut db)?
            .ok_or(error::anyhow!("Error loading wallet"))?;
        let descriptor = wallet.public_descriptor(KeychainKind::Internal);
        log::info!("descriptor: {descriptor}");
        Ok((wallet, ldk_keys))
    }

    #[cfg(debug_assertions)]
    async fn build_from_private_key(
        conf: Arc<LampoConf>,
        xprv: PrivateKey,
        channel_keys: Option<String>,
    ) -> error::Result<(PersistedWallet<Store<ChangeSet>>, LampoKeys)> {
        use bdk_wallet::bitcoin::bip32::ExtendedPrivKey;

        let ldk_keys = if channel_keys.is_some() {
            LampoKeys::with_channel_keys(xprv.inner.secret_bytes(), channel_keys.unwrap())
        } else {
            LampoKeys::new(xprv.inner.secret_bytes())
        };

        let mut db = Store::<ChangeSet>::open_or_create_new(
            "lampo".as_bytes(),
            format!("{}/bdk-wallet.db", conf.path()),
        )?;

        let key = ExtendedPrivKey::new_master(conf.network, &xprv.inner.secret_bytes())?;
        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let wallet = Wallet::load()
            .descriptor(
                bdk_wallet::KeychainKind::External,
                Some(Bip84(ExtendedKey::from(key), KeychainKind::External)),
            )
            .descriptor(
                bdk_wallet::KeychainKind::Internal,
                Some(Bip84(ExtendedKey::from(key), KeychainKind::Internal)),
            )
            .extract_keys()
            .check_network(conf.network)
            .load_wallet(&mut db)?
            .ok_or(error::anyhow!("Error loading wallet"))?;
        let descriptor = wallet.public_descriptor(KeychainKind::Internal);
        log::info!("descriptor: {descriptor}");
        Ok((wallet, ldk_keys))
    }

    pub fn build_client(conf: Arc<LampoConf>) -> error::Result<Client> {
        let url = conf.core_url.as_ref().ok_or(error::anyhow!(
            "RPC URL is missing from the configuration file"
        ))?;
        let user = conf.core_user.as_ref().ok_or(error::anyhow!(
            "RPC User is missing from the configuration file"
        ))?;
        let pass = conf.core_pass.as_ref().ok_or(error::anyhow!(
            "RPC Password is missing from the configuration file"
        ))?;
        let client = Client::new(url, Auth::UserPass(user.clone(), pass.clone()))?;
        Ok(client)
    }
}

#[async_trait]
impl WalletManager for BDKWalletManager {
    async fn new(conf: Arc<LampoConf>) -> error::Result<(Self, String)> {
        // Generate fresh mnemonic
        let mnemonic: GeneratedKey<_, bdk_wallet::miniscript::Tap> =
            Mnemonic::generate((WordCount::Words12, Language::English)).unwrap();
        // Convert mnemonic to string
        let mnemonic_words = mnemonic.to_string();
        log::info!("mnemonic words `{mnemonic_words}`");
        let (wallet, keymanager) = Self::build_wallet(conf.clone(), &mnemonic_words).await?;
        let client = Self::build_client(conf.clone())?;
        Ok((
            Self {
                wallet: RefCell::new(Mutex::new(wallet)),
                keymanager: Arc::new(keymanager),
                network: conf.network,
                rpc: Arc::new(client),
            },
            mnemonic_words,
        ))
    }

    async fn restore(conf: Arc<LampoConf>, mnemonic_words: &str) -> error::Result<Self> {
        let (wallet, keymanager) =
            BDKWalletManager::build_wallet(conf.clone(), mnemonic_words).await?;
        let client = BDKWalletManager::build_client(conf.clone())?;
        Ok(Self {
            wallet: RefCell::new(Mutex::new(wallet)),
            keymanager: Arc::new(keymanager),
            network: conf.network,
            rpc: Arc::new(client),
        })
    }

    fn ldk_keys(&self) -> Arc<LampoKeys> {
        self.keymanager.clone()
    }

    async fn get_onchain_address(&self) -> error::Result<NewAddress> {
        let address = self
            .wallet
            .borrow_mut()
            .lock()
            .unwrap()
            .reveal_next_address(KeychainKind::External);
        Ok(NewAddress {
            address: address.address.to_string(),
        })
    }

    // Return in satoshis
    async fn get_onchain_balance(&self) -> error::Result<u64> {
        self.sync().await?;
        let balance = self.wallet.borrow().lock().unwrap().balance();
        Ok(balance.confirmed.to_sat())
    }

    async fn create_transaction(
        &self,
        script: ScriptBuf,
        amount: u64,
        fee_rate: u32,
    ) -> error::Result<Transaction> {
        self.sync().await?;
        let wallet = self.wallet.borrow_mut();
        let mut wallet = wallet.lock().unwrap();
        let mut tx = wallet.build_tx();
        tx.add_recipient(
            ScriptBuf::from_bytes(script.into_bytes()),
            Amount::from_sat(amount),
        )
        .fee_rate(FeeRate::from_sat_per_vb_unchecked(fee_rate as u64));
        let mut psbt = tx.finish()?;
        if !wallet.sign(&mut psbt, SignOptions::default())? {
            error::bail!("wallet not able to sing the psbt {psbt}");
        }
        if !wallet.finalize_psbt(&mut psbt, SignOptions::default())? {
            error::bail!("wallet impossible finalize the psbt: {psbt}");
        };
        Ok(psbt.extract_tx()?)
    }

    async fn list_transactions(&self) -> error::Result<Vec<Utxo>> {
        self.sync().await?;
        let wallet = self.wallet.borrow();
        let wallet = wallet.lock().unwrap();
        let txs = wallet
            .list_unspent()
            .map(|tx| Utxo {
                txid: tx.outpoint.txid.to_string(),
                vout: tx.outpoint.vout,
                reserved: tx.is_spent,
                confirmed: 0,
                amount_msat: tx.txout.value.to_sat() * 1000_u64,
            })
            .collect::<Vec<_>>();
        Ok(txs)
    }

    async fn sync(&self) -> error::Result<()> {
        #[derive(Debug)]
        enum Emission {
            SigTerm,
            Block(bdk_bitcoind_rpc::BlockEvent<Block>),
            Mempool(Vec<(Transaction, u64)>),
        }

        let wallet = self.wallet.borrow();
        let wallet = wallet.lock().unwrap();

        let (sender, receiver) = channel::<Emission>();

        let signal_sender = sender.clone();
        /*ctrl_c::set_handler(move || {
            signal_sender
                .send(Emissine::SigTerm)
                .expect("failed to send sigterm")
        });*/

        let tip = wallet.latest_checkpoint();
        let emitter_tip = tip.clone();
        let rpc_client = self.rpc.clone();
        // FIXME: We need to add this inside a listen method, so in this way we can drop the sync
        std::thread::spawn(move || -> error::Result<()> {
            let height = emitter_tip.height();
            let mut emitter = Emitter::new(rpc_client.as_ref(), emitter_tip, height);
            while let Some(emission) = emitter.next_block()? {
                sender.send(Emission::Block(emission))?;
            }
            sender.send(Emission::Mempool(emitter.mempool()?))?;
            Ok(())
        });
        Ok(())
    }
}

/*
#[cfg(debug_assertions)]
impl TryFrom<(PrivateKey, Option<String>)> for BDKWalletManager {
    type Error = bdk::Error;

    fn try_from(value: (PrivateKey, Option<String>)) -> Result<Self, Self::Error> {
        let (wallet, keymanager) = BDKWalletManager::build_from_private_key(value.0, value.1)?;
        Ok(Self {
            wallet: RefCell::new(Mutex::new(wallet)),
            keymanager: Arc::new(keymanager),
            // This should be possible only during integration testing
            // FIXME: fix the sync method in bdk, the esplora client will crash!
            network: Network::Regtest,
        })
    }
}   */
