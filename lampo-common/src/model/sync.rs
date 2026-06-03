pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    /// Snapshot of the node's chain-sync progress, returned by `sync_wallets`
    /// once the initial sync completes.
    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct SyncStatus {
        pub chain_listeners_synced: bool,
        pub initial_sync_complete: bool,
        pub sync_in_progress: bool,
        pub wallet_scan_height: Option<u32>,
    }
}
