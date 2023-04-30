//! Chain module implementation that contains all the code related to the blockchain communication.

mod blockchain;
mod wallet;

pub use bitcoin::Network;

pub use blockchain::LampoChainManager;
pub use wallet::{LampoWalletManager, WalletManager};
