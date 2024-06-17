//! Chain module implementation that contains all the code related to the blockchain communication.
mod blockchain;

pub use lampo_common::btc::bitcoin::Network;
pub use lampo_common::wallet::WalletManager;

pub use blockchain::LampoChainManager;
