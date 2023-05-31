use std::{sync::Arc, time::SystemTime};

use lampo_common::keymanager::KeysManager;

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

    #[cfg(debug_assertions)]
    pub fn with_channel_keys(seed: [u8; 32], channels_keys: String) -> Self {
        // Fill in random_32_bytes with secure random data, or, on restart, reload the seed from disk.
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        let keys = channels_keys.split("/").collect::<Vec<_>>();

        let mut manager = KeysManager::new(&seed, start_time.as_secs(), start_time.subsec_nanos());
        manager.set_channels_keys(
            keys[0].to_string(),
            keys[1].to_string(),
            keys[2].to_string(),
            keys[3].to_string(),
            keys[4].to_string(),
            keys[5].to_string(),
        );
        LampoKeys {
            keys_manager: Arc::new(manager),
        }
    }

    pub fn inner(&self) -> Arc<KeysManager> {
        self.keys_manager.clone()
    }
}
