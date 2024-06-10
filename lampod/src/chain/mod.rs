//! Chain module implementation that contains all the code related to the blockchain communication.
mod blockchain;

#[cfg(feature = "vanilla")]
pub use {lampo_common::bitcoin::Network, lampo_common::wallet::WalletManager};

#[cfg(feature = "rgb")]
pub use {rgb_lampo_common::bitcoin::Network, rgb_lampo_common::wallet::WalletManager};

pub use blockchain::LampoChainManager;
