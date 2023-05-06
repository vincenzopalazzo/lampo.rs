use lightning::sign::KeysManager;

use std::{sync::Arc, time::SystemTime};

/// Lampo keys implementations
pub struct LampoKeys {
    pub(crate) keys_manager: Arc<KeysManager>,
}

impl LampoKeys {
    pub fn new(seed: [u8; 32]) -> Self {
        // Fill in random_32_bytes with secure random data, or, on restart, reload the seed from disk.
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        LampoKeys {
            keys_manager: Arc::new(KeysManager::new(
                &seed,
                start_time.as_secs(),
                start_time.subsec_nanos(),
            )),
        }
    }

    pub fn inner(&self) -> Arc<KeysManager> {
        self.keys_manager.clone()
    }
}
