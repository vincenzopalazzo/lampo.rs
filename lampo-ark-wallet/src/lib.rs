use std::sync::Arc;

use lampo_bdk_wallet::BDKWalletManager;
use lampo_chain::LampoChainSync;
use lampo_common::backend::{AsyncBlockSourceResult, Backend, BlockData, BlockHeaderData};
use lampo_common::bitcoin::PublicKey;
use lampo_common::bitcoin::absolute::Height;
use lampo_common::bitcoin::{
    Amount, BlockHash, FeeRate, Script, ScriptBuf, Sequence, Transaction, XOnlyPublicKey,
    opcodes::all::{
        OP_CHECKMULTISIG, OP_CHECKSIG, OP_CHECKSIGVERIFY, OP_CSV, OP_DROP, OP_PUSHNUM_1,
        OP_PUSHNUM_2, OP_PUSHNUM_3,
    },
    secp256k1::Secp256k1,
    taproot::TaprootBuilder,
};
use lampo_common::conf::LampoConf;
use lampo_common::keys::LampoKeys;
use lampo_common::ldk::block_sync::BlockSource;
use lampo_common::ldk::ln::chan_utils::ChannelTransactionParameters;
use lampo_common::model::response::{NewAddress, Utxo};
use lampo_common::types::{LampoChainMonitor, LampoChannel};
use lampo_common::wallet::WalletManager;
use lampo_common::{async_trait, bitcoin, error};
use serde::Deserialize;

pub const UNSPENDABLE_KEY: &str =
    "0250929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0";

#[derive(Debug, Deserialize)]
struct ArkServerInfo {
    pubkey: String,
    #[serde(rename = "unilateralExitDelay")]
    unilateral_exit_delay: String,
    #[serde(rename = "roundInterval")]
    round_interval: String,
    network: String,
    dust: String,
    version: String,
}

pub struct LampoArkWallet {
    pub inner: Arc<BDKWalletManager>,
    pub backend: Arc<LampoChainSync>,
    pub server_pk: XOnlyPublicKey,
    pub timelock: Sequence,
    pub lampo_conf: Arc<LampoConf>,
}

impl LampoArkWallet {
    /// Fetch ark server info from the configured ark server API
    async fn fetch_ark_server_info(ark_server_url: &str) -> error::Result<ArkServerInfo> {
        let info_url = if ark_server_url.ends_with('/') {
            format!("{}v1/info", ark_server_url)
        } else {
            format!("{}/v1/info", ark_server_url)
        };

        log::info!("Fetching ark server info from: {}", info_url);

        let client = reqwest::Client::new();
        let response = client
            .get(&info_url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch ark server info: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Ark server returned error status: {}",
                response.status()
            ));
        }

        let server_info: ArkServerInfo = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse ark server info: {}", e))?;

        log::info!("Successfully fetched ark server info: {:?}", server_info);
        Ok(server_info)
    }

    /// Parse the ark server info and extract server pubkey and timelock
    fn parse_ark_server_info(
        server_info: &ArkServerInfo,
    ) -> error::Result<(XOnlyPublicKey, Sequence)> {
        // Parse the server public key
        let server_pk_bytes = hex::decode(&server_info.pubkey)
            .map_err(|e| anyhow::anyhow!("Invalid server pubkey hex: {}", e))?;

        let server_pk = XOnlyPublicKey::from_slice(&server_pk_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid server pubkey: {}", e))?;

        // Parse the unilateral exit delay
        // The API returns this in blocks, but we need to convert to 24 hours (144 blocks for 10min blocks)
        let _exit_delay_blocks: u32 = server_info
            .unilateral_exit_delay
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid unilateral exit delay: {}", e))?;

        // Set timelock to 24 hours (144 blocks assuming 10 minute blocks)
        let timelock = Sequence::from_height(144);

        log::info!(
            "Parsed server pk: {} and timelock: {} blocks",
            server_info.pubkey,
            144
        );
        Ok((server_pk, timelock))
    }

    pub fn multisig_script(
        pk_0: XOnlyPublicKey,
        pk_1: XOnlyPublicKey,
        server: XOnlyPublicKey,
    ) -> ScriptBuf {
        ScriptBuf::builder()
            .push_x_only_key(&pk_0)
            .push_opcode(OP_CHECKSIGVERIFY)
            .push_x_only_key(&pk_1)
            .push_opcode(OP_CHECKSIGVERIFY)
            .push_x_only_key(&server)
            .push_opcode(OP_CHECKSIG)
            .into_script()
    }

    pub fn csv_sig_script(
        locktime: Sequence,
        alice: XOnlyPublicKey,
        bob: XOnlyPublicKey,
    ) -> ScriptBuf {
        ScriptBuf::builder()
            .push_int(locktime.to_consensus_u32() as i64)
            .push_opcode(OP_CSV)
            .push_opcode(OP_DROP)
            .push_x_only_key(&alice)
            .push_opcode(OP_CHECKSIGVERIFY)
            .push_x_only_key(&bob)
            .push_opcode(OP_CHECKSIG)
            .into_script()
    }
}

#[async_trait]
impl WalletManager for LampoArkWallet {
    async fn new(conf: Arc<LampoConf>) -> error::Result<(Self, String)> {
        let (wallet, mnemonic_words) = BDKWalletManager::new(conf.clone()).await?;
        let backend = LampoChainSync::new(conf.clone())?;

        // Fetch ark server info if configured
        let (server_pk, timelock) = if let Some(ark_server_url) = &conf.ark_server_api {
            log::info!("Ark server API configured: {}", ark_server_url);
            let server_info = Self::fetch_ark_server_info(ark_server_url).await?;
            log::info!("Successfully fetched ark server info");
            Self::parse_ark_server_info(&server_info)?
        } else {
            anyhow::bail!("No ark server API configured");
        };

        Ok((
            Self {
                inner: Arc::new(wallet),
                backend: Arc::new(backend),
                server_pk,
                timelock,
                lampo_conf: conf.clone(),
            },
            mnemonic_words,
        ))
    }

    fn build_funding_transaction(
        &self,
        channels_keys: &ChannelTransactionParameters,
    ) -> error::Result<ScriptBuf> {
        // Alice checksigverify Bob checksigverify Server checksig
        let alice_pk = channels_keys.holder_pubkeys.funding_pubkey.serialize();
        let bob_pk = channels_keys
            .counterparty_parameters
            .as_ref()
            .unwrap()
            .pubkeys
            .funding_pubkey;
        let bob_pk = bob_pk.serialize();
        let alice_pk = XOnlyPublicKey::from_slice(&alice_pk)?;
        let bob_pk = XOnlyPublicKey::from_slice(&bob_pk)?;
        let forfeit_script =
            Self::multisig_script(alice_pk.clone(), bob_pk.clone(), self.server_pk);
        // timelock CSV drop Alice checksigverify Bob checksig
        let redeem_script = Self::csv_sig_script(self.timelock, alice_pk, bob_pk);

        let unspendable_key: PublicKey = UNSPENDABLE_KEY.parse().expect("valid key");
        let (unspendable_key, _) = unspendable_key.inner.x_only_public_key();

        let secp = Secp256k1::new();

        let script = TaprootBuilder::new()
            .add_leaf(1, forfeit_script)
            .expect("valid forfeit leaf")
            .add_leaf(1, redeem_script)
            .expect("valid redeem leaf")
            .finalize(&secp, unspendable_key)
            .expect("can be finalized");

        let output_key = script.output_key();
        let builder = bitcoin::blockdata::script::Builder::new();
        let script = builder
            .push_opcode(OP_PUSHNUM_1)
            .push_slice(output_key.serialize())
            .into_script();
        Ok(script)
    }

    async fn restore(conf: Arc<LampoConf>, mnemonic_words: &str) -> error::Result<Self> {
        let wallet = BDKWalletManager::restore(conf.clone(), mnemonic_words).await?;
        let backend = LampoChainSync::new(conf.clone())?;

        // Fetch ark server info if configured
        let (server_pk, timelock) = if let Some(ark_server_url) = &conf.ark_server_api {
            log::info!("Ark server API configured: {}", ark_server_url);
            match Self::fetch_ark_server_info(ark_server_url).await {
                Ok(server_info) => {
                    log::info!("Successfully fetched ark server info");
                    Self::parse_ark_server_info(&server_info)?
                }
                Err(e) => {
                    log::warn!("Failed to fetch ark server info, using defaults: {}", e);
                    // Fall back to default hardcoded values
                    let server_pk = XOnlyPublicKey::from_slice(
                        b"0250929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0",
                    )?;
                    let timelock = Sequence::from_height(10);
                    (server_pk, timelock)
                }
            }
        } else {
            log::info!("No ark server API configured, using defaults");
            // Use default hardcoded values
            let server_pk = XOnlyPublicKey::from_slice(
                b"0250929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0",
            )?;
            let timelock = Sequence::from_height(10);
            (server_pk, timelock)
        };

        Ok(Self {
            inner: Arc::new(wallet),
            backend: Arc::new(backend),
            server_pk,
            timelock,
            lampo_conf: conf.clone(),
        })
    }

    fn ldk_keys(&self) -> Arc<LampoKeys> {
        self.inner.ldk_keys()
    }

    async fn get_onchain_address(&self) -> error::Result<NewAddress> {
        self.inner.get_onchain_address().await
    }

    async fn get_onchain_balance(&self) -> error::Result<u64> {
        self.inner.get_onchain_balance().await
    }

    async fn create_transaction(
        &self,
        script: ScriptBuf,
        amount: Amount,
        fee_rate: FeeRate,
        best_block: Height,
    ) -> error::Result<Transaction> {
        // FIXME: this need to build a magic ARK funding transaction
        self.inner
            .create_transaction(script, amount, fee_rate, best_block)
            .await
    }

    async fn list_transactions(&self) -> error::Result<Vec<Utxo>> {
        self.inner.list_transactions().await
    }

    async fn sync(&self) -> error::Result<()> {
        self.inner.sync().await
    }

    async fn wallet_tips(&self) -> error::Result<Height> {
        self.inner.wallet_tips().await
    }

    async fn listen(self: Arc<Self>) -> error::Result<()> {
        self.inner.clone().listen().await
    }
}

#[async_trait]
impl Backend for LampoArkWallet {
    fn kind(&self) -> lampo_common::backend::BackendKind {
        lampo_common::backend::BackendKind::Core
    }

    async fn brodcast_tx(&self, tx: &Transaction) {
        self.backend.brodcast_tx(tx).await;
    }

    async fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32> {
        self.backend.fee_rate_estimation(blocks).await
    }

    async fn get_transaction(
        &self,
        txid: &lampo_common::bitcoin::Txid,
    ) -> error::Result<lampo_common::backend::TxResult> {
        self.backend.get_transaction(txid).await
    }

    async fn get_utxo(&self, block: &BlockHash, idx: u64) -> lampo_common::backend::UtxoResult {
        self.backend.get_utxo(block, idx).await
    }

    async fn get_utxo_by_txid(
        &self,
        txid: &lampo_common::bitcoin::Txid,
        script: &lampo_common::bitcoin::Script,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        self.backend.get_utxo_by_txid(txid, script).await
    }

    async fn minimum_mempool_fee(&self) -> lampo_common::error::Result<u32> {
        self.backend.minimum_mempool_fee().await
    }

    fn set_handler(&self, handler: Arc<dyn lampo_common::handler::Handler>) {
        self.backend.set_handler(handler);
    }

    fn set_channel_manager(&self, channel_manager: Arc<LampoChannel>) {
        self.backend.set_channel_manager(channel_manager);
    }

    fn set_chain_monitor(&self, chain_monitor: Arc<LampoChainMonitor>) {
        self.backend.set_chain_monitor(chain_monitor);
    }

    async fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
        self.backend.clone().listen().await
    }
}

impl BlockSource for LampoArkWallet {
    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData> {
        self.backend.get_header(header_hash, height_hint)
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a BlockHash,
    ) -> AsyncBlockSourceResult<'a, BlockData> {
        self.backend.get_block(header_hash)
    }

    fn get_best_block<'a>(&'a self) -> AsyncBlockSourceResult<(BlockHash, Option<u32>)> {
        self.backend.get_best_block()
    }
}
