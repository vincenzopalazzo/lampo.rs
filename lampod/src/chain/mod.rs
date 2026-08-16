//! Chain module implementation that contains all the code related to the blockchain communication.
mod blockchain;
mod fee;

pub use lampo_common::bitcoin::Network;
pub use lampo_common::wallet::WalletManager;

pub use blockchain::LampoChainManager;
pub use fee::FeeTarget;
