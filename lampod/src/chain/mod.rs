//! Chain module implementation that contains all the code related to the blockchain communication.

mod blockchain;

pub use bitcoin::Network;

pub use blockchain::LampoChainManager;
pub use lampo_common::wallet::WalletManager;
