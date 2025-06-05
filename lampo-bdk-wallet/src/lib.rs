//! Wallet Manager implementation with BDK
use std::str::FromStr;
use std::sync::Arc;

use bdk_bitcoind_rpc::bitcoincore_rpc::{Auth, Client, RpcApi};
use bdk_bitcoind_rpc::Emitter;
use bdk_wallet::chain::BlockId;
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::keys::bip39::{Language, WordCount};
use bdk_wallet::keys::{DerivableKey, ExtendedKey, GeneratableKey, GeneratedKey};
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::template::Bip84;
use bdk_wallet::{KeychainKind, PersistedWallet, SignOptions, Wallet};
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tokio::time::Instant;
use tokio_cron_scheduler::{Job, JobScheduler};

use lampo_common::bitcoin::absolute::Height;
use lampo_common::bitcoin::bip32::Xpriv;
use lampo_common::bitcoin::blockdata::locktime::absolute::LockTime;
use lampo_common::bitcoin::PrivateKey;
use lampo_common::bitcoin::{Amount, Block, FeeRate, ScriptBuf, Transaction};
use lampo_common::conf::{LampoConf, Network};
use lampo_common::keys::LampoKeys;
use lampo_common::model::response::NewAddress;
use lampo_common::model::response::Utxo;
use lampo_common::secp256k1::SecretKey;
use lampo_common::wallet::WalletManager;
use lampo_common::{async_trait, error};

pub struct BDKWalletManager {
    pub wallet: Mutex<PersistedWallet<Connection>>,
    pub wallet_db: Mutex<Connection>,
    pub rpc: Arc<Client>,
    pub keymanager: Arc<LampoKeys>,
    pub network: Network,
    pub reindex_from: Option<Height>,
    pub conf: Arc<LampoConf>,

    guard: Mutex<bool>,
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
    ) -> error::Result<(PersistedWallet<Connection>, Connection, LampoKeys)> {
        if let Some(ref priv_key) = conf.private_key {
            log::warn!(target: "lampo-wallet", "Using a private key to create the wallet");
            let key = SecretKey::from_str(priv_key)?;
            log::info!(target: "lampo-wallet", "Using a private key for network {:?}", conf.network);
            let key = PrivateKey::new(key, conf.network);
            let channels_keys = conf.channels_keys.clone();
            log::info!(target: "lampo-wallet", "channels_keys: {channels_keys:?}");
            log::info!(target: "lampo-wallet", "key: {key:?}");
            assert!(channels_keys.is_some());
            return Self::build_from_private_key(conf, key, channels_keys).await;
        }

        // Parse a mnemonic
        let mnemonic = Mnemonic::parse(mnemonic_words)?;
        // Generate the extended key
        let xkey: ExtendedKey = mnemonic.into_extended_key()?;
        // Get xprv from the extended key
        let xprv = xkey
            .into_xprv(conf.network)
            .ok_or(error::anyhow!("Error converting xpriv"))?;

        let path_db = format!("{}/bdk-wallet.db", conf.path());
        let mut db = Connection::open(path_db)?;

        let internal_descriptor = Bip84(xprv, KeychainKind::Internal);
        let external_descriptor = Bip84(xprv, KeychainKind::External);

        let ldk_keys = LampoKeys::new(xprv.private_key.secret_bytes());
        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let wallet = Wallet::load()
            .descriptor(
                bdk_wallet::KeychainKind::External,
                Some(external_descriptor.clone()),
            )
            .descriptor(
                bdk_wallet::KeychainKind::Internal,
                Some(internal_descriptor.clone()),
            )
            .extract_keys()
            .check_network(conf.network)
            .load_wallet(&mut db)?;

        let wallet = match wallet {
            Some(wallet) => wallet,
            None => Wallet::create(external_descriptor, internal_descriptor)
                .network(conf.network)
                .create_wallet(&mut db)?,
        };
        let descriptor = wallet.public_descriptor(KeychainKind::Internal);
        log::info!("descriptor: {descriptor}");
        Ok((wallet, db, ldk_keys))
    }

    // FIXME: put this under a cfg
    async fn build_from_private_key(
        conf: Arc<LampoConf>,
        xprv: PrivateKey,
        channel_keys: Option<String>,
    ) -> error::Result<(PersistedWallet<Connection>, Connection, LampoKeys)> {
        let ldk_keys = if channel_keys.is_some() {
            LampoKeys::with_channel_keys(xprv.inner.secret_bytes(), channel_keys.unwrap())
        } else {
            LampoKeys::new(xprv.inner.secret_bytes())
        };

        let mut db = Connection::open_in_memory()?;

        let xpriv = Xpriv::new_master(conf.network, &xprv.inner.secret_bytes())?;

        let internal_descriptor = Bip84(xpriv, KeychainKind::Internal);
        let external_descriptor = Bip84(xpriv, KeychainKind::External);

        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let wallet = Wallet::load()
            .descriptor(
                bdk_wallet::KeychainKind::External,
                Some(external_descriptor.clone()),
            )
            .descriptor(
                bdk_wallet::KeychainKind::Internal,
                Some(internal_descriptor.clone()),
            )
            .extract_keys()
            .check_network(conf.network)
            .load_wallet(&mut db)?;

        let wallet = match wallet {
            Some(wallet) => wallet,
            None => Wallet::create(external_descriptor, internal_descriptor)
                .network(conf.network)
                .create_wallet(&mut db)?,
        };

        let descriptor = wallet.public_descriptor(KeychainKind::Internal);
        log::info!("descriptor: {descriptor}");
        Ok((wallet, db, ldk_keys))
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
        let (wallet, db, keymanager) = Self::build_wallet(conf.clone(), &mnemonic_words).await?;
        let client = Self::build_client(conf.clone())?;
        Ok((
            Self {
                wallet: Mutex::new(wallet),
                wallet_db: Mutex::new(db),
                keymanager: Arc::new(keymanager),
                network: conf.network,
                rpc: Arc::new(client),
                guard: Mutex::new(false),
                reindex_from: conf.reindex,
                conf: conf.clone(),
            },
            mnemonic_words,
        ))
    }

    async fn restore(conf: Arc<LampoConf>, mnemonic_words: &str) -> error::Result<Self> {
        let (wallet, db, keymanager) =
            BDKWalletManager::build_wallet(conf.clone(), mnemonic_words).await?;
        let client = BDKWalletManager::build_client(conf.clone())?;
        Ok(Self {
            wallet: Mutex::new(wallet),
            wallet_db: Mutex::new(db),
            keymanager: Arc::new(keymanager),
            network: conf.network,
            rpc: Arc::new(client),
            guard: Mutex::new(false),
            reindex_from: conf.reindex,
            conf: conf.clone(),
        })
    }

    fn ldk_keys(&self) -> Arc<LampoKeys> {
        self.keymanager.clone()
    }

    async fn get_onchain_address(&self) -> error::Result<NewAddress> {
        let mut wallet = self.wallet.lock().await;
        let mut wallet_db = self.wallet_db.lock().await;
        let address = wallet.reveal_next_address(KeychainKind::External);
        wallet.persist(&mut wallet_db)?;
        Ok(NewAddress {
            address: address.address.to_string(),
        })
    }

    // Return in satoshis
    async fn get_onchain_balance(&self) -> error::Result<u64> {
        let balance = self.wallet.lock().await.balance();
        log::warn!(target: "lampo-wallet", "balance: {balance:?}");
        Ok(balance.confirmed.to_sat())
    }

    async fn create_transaction(
        &self,
        script: ScriptBuf,
        amount: Amount,
        fee_rate: FeeRate,
        best_block: Height,
    ) -> error::Result<Transaction> {
        let mut wallet = self.wallet.lock().await;

        // We set nLockTime to the current height to discourage fee sniping.
        let locktime =
            LockTime::from_height(best_block.to_consensus_u32()).unwrap_or(LockTime::ZERO);

        let mut tx = wallet.build_tx();
        tx.add_recipient(script, amount)
            .fee_rate(fee_rate)
            .nlocktime(locktime);
        let mut psbt = tx.finish()?;
        let opts = SignOptions::default();
        if !wallet.sign(&mut psbt, opts.clone())? {
            error::bail!("wallet not able to sing the psbt {psbt}");
        }
        Ok(psbt.extract_tx()?)
    }

    async fn list_transactions(&self) -> error::Result<Vec<Utxo>> {
        log::info!("lampo-wallet: list transactions");
        let wallet = self.wallet.lock().await;
        log::info!("lampo-wallet: wallet lock taken");
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

        log::info!(target: "lampo-wallet", "Checking the wallet status...");
        let (sender, mut receiver) = unbounded_channel::<Emission>();

        /*let signal_sender = sender.clone();
        ctrl_c::set_handler(move || {
            signal_sender
                .send(Emissine::SigTerm)
                .expect("failed to send sigterm")
        });*/

        let rpc_client = self.rpc.clone();
        let wallet = self.wallet.lock().await;
        let wallet_tip = wallet.latest_checkpoint();
        let start_height = wallet_tip.height();
        drop(wallet);

        tokio::spawn(async move {
            let mut emitter = Emitter::new(rpc_client.as_ref(), wallet_tip, start_height);

            while let Some(emission) = emitter.next_block()? {
                sender.send(Emission::Block(emission))?;
            }
            //sender.send(Emission::Mempool(emitter.mempool()?))?;
            Ok::<_, error::Error>(())
        });

        while let Some(emission) = receiver.recv().await {
            let mut wallet = self.wallet.lock().await;
            let mut wallet_db = self.wallet_db.lock().await;

            match emission {
                Emission::SigTerm => {
                    println!("Sigterm received, exiting...");
                    break;
                }
                Emission::Block(block_emission) => {
                    let height = block_emission.block_height();
                    let hash = block_emission.block_hash();
                    let connected_to = block_emission.connected_to();
                    let start_apply_block = Instant::now();
                    wallet.apply_block_connected_to(&block_emission.block, height, connected_to)?;
                    wallet.persist(&mut wallet_db)?;
                    let elapsed = start_apply_block.elapsed().as_secs_f32();
                    log::info!(target: "lampo-wallet",
                        "Applied block {} at height {} in {}s",
                        hash, height, elapsed
                    );
                }
                Emission::Mempool(mempool_emission) => {
                    log::warn!(target: "lampo-wallet", "Mempool emission: {mempool_emission:?}");
                    let start_apply_mempool = Instant::now();
                    wallet.apply_unconfirmed_txs(mempool_emission);
                    wallet.persist(&mut wallet_db)?;
                    log::info!(target: "lampo-wallet",
                        "Applied unconfirmed transactions in {}s",
                        start_apply_mempool.elapsed().as_secs_f32()
                    );
                    break;
                }
            }
        }
        // FIXME: update the wallet status!
        Ok(())
    }

    async fn wallet_tips(&self) -> error::Result<Height> {
        let wallet = self.wallet.lock().await;
        let tip = wallet.latest_checkpoint().height();
        let tip = Height::from_consensus(tip)?;
        Ok(tip)
    }

    async fn listen(self: Arc<Self>) -> error::Result<()> {
        let sched = JobScheduler::new().await?;
        sched.shutdown_on_ctrl_c();

        async fn innet_sync(wallet: Arc<BDKWalletManager>) -> error::Result<()> {
            let _is_sync = wallet.guard.lock().await;
            log::debug!(target: "lampo-wallet", "Tick tock, time to check if we need to sync the wallet");
            wallet.sync().await?;
            Ok(())
        }

        // we need to modify the bdk wallet state in this position.
        let rpc_client = self.rpc.clone();
        let mut wallet = self.wallet.lock().await;
        let wallet_tip = wallet.latest_checkpoint();
        let start_height = wallet_tip.height();

        let reindex_from = self.reindex_from.or_else(|| {
            if start_height == 0 {
                rpc_client.get_blockchain_info().ok().map(|info| {
                    Height::from_consensus(info.blocks as u32)
                        .expect("Failed to convert blockchain height to consensus height")
                })
            } else {
                None
            }
        });

        // Instruct the wallet to reindex from the specified height, ensuring it starts scanning the blockchain from this point onward.
        if let Some(height) = reindex_from {
            let height = height.to_consensus_u32();
            if height > wallet_tip.height() {
                // Insert a checkpoint into the wallet to avoid scanning the entire chain.
                let hash = rpc_client.get_block_hash(height as u64)?;
                let block = BlockId { height, hash };
                let new_tip = wallet_tip.insert(block);
                let update = bdk_wallet::Update {
                    chain: Some(new_tip),
                    ..Default::default()
                };
                wallet.apply_update(update)?;
            }
        }

        let wallet = self.clone();

        // Determine the sync schedule based on dev_sync configuration
        let sync_schedule = if self.conf.dev_sync.unwrap_or(false) {
            log::info!(target: "lampo-wallet", "Using development sync schedule: every second");
            "* * * * * *" // Every second for development
        } else {
            log::info!(target: "lampo-wallet", "Using production sync schedule: every 2 minutes");
            "0 */2 * * * *" // Every 2 minutes for production (original schedule)
        };

        let job = Job::new_async_tz(sync_schedule, chrono::Utc, move |_uuid, _l| {
            let wallet = wallet.clone();
            Box::pin(async move {
                if let Err(err) = wallet.guard.try_lock() {
                    log::info!(target: "lampo-wallet", "Already syncing the wallet, skipping this round");
                    log::debug!(target: "lampo-wallet", "Unable to take the log: {err}");
                    return;
                }
                let Err(err) = innet_sync(wallet).await else {
                    return;
                };
                log::error!("Error during the sync: {err}");
            })
        })?;
        sched.add(job).await?;

        let wallet = self.clone();
        let one_shot = Job::new_one_shot_async(Duration::from_secs(5), move |_, _| {
            let wallet = wallet.clone();
            Box::pin(async move {
                if let Err(err) = wallet.guard.try_lock() {
                    log::info!(target: "lampo-wallet", "Already syncing the wallet, skipping this round");
                    log::debug!(target: "lampo-wallet", "Unable to take the log: {err}");
                    return;
                }
                let Err(err) = innet_sync(wallet).await else {
                    return;
                };
                log::error!("Error during the sync: {err}");
            })
        })?;
        sched.add(one_shot).await?;

        sched.start().await?;
        Ok(())
    }
}
