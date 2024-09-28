//! Chain module implementation that contains all the code related to the
//! blockchain communication.
mod blockchain;

pub use lampo_common::bitcoin::Network;
pub use lampo_common::wallet::WalletManager;

pub use blockchain::LampoChainManager;
