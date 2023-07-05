//! Lampo test framework.
pub mod prelude {
    pub use cln4rust_testing::*;
}

use std::sync::Arc;

use cln4rust_testing::btc::BtcNode;
use cln4rust_testing::prelude::*;
use lampo_bitcoind::BitcoinCore;
use lampo_common::conf::LampoConf;
use tempfile::TempDir;

use lampo_common::error;
use lampod::chain::{LampoWalletManager, WalletManager};
use lampod::LampoDeamon;

pub struct LampoTesting {
    inner: LampoDeamon,
    wallet: Arc<LampoWalletManager>,
    mnemonic: String,
    root_path: Arc<TempDir>,
}

impl LampoTesting {
    pub async fn new(btc: &BtcNode) -> error::Result<Self> {
        let dir = tempfile::tempdir()?;

        let lampo_conf = LampoConf::new(
            dir.path().to_str().unwrap(),
            lampo_common::bitcoin::Network::Regtest,
            port::random_free_port().unwrap().into(),
        );
        let (wallet, mnemonic) = LampoWalletManager::new(Arc::new(lampo_conf.clone()))?;
        let wallet = Arc::new(wallet);
        let mut lampo = LampoDeamon::new(lampo_conf, wallet.clone());
        let node = BitcoinCore::new(&format!("127.0.0.1:{}", btc.port), &btc.user, &btc.pass)?;
        lampo.init(Arc::new(node))?;
        Ok(Self {
            inner: lampo,
            mnemonic,
            wallet: wallet.clone(),
            root_path: Arc::new(dir),
        })
    }

    pub fn lampod(&self) -> &LampoDeamon {
        &self.inner
    }

    pub fn root_path(&self) -> Arc<TempDir> {
        self.root_path.clone()
    }
}
