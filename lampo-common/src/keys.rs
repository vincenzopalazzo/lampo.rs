use std::{sync::Arc, time::SystemTime};

use crate::ldk::sign::KeysManager;
#[cfg(feature = "rgb")]
pub use {
    std::{path::PathBuf, str::FromStr},
    crate::conf::LampoConf,
};

/// Lampo keys implementations
pub struct LampoKeys {
    pub keys_manager: Arc<KeysManager>,
}

impl LampoKeys {
    #[cfg(feature = "vanilla")]
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

    // We need a different function definition for this function as the previous version of `lightning`
    // keysmanager takes also the path_dir as an argument.
    #[cfg(feature = "rgb")]
    pub fn new(seed: [u8; 32], conf: Arc<LampoConf>) -> Self {
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        let path = format!("{}/{}/rgb/", conf.root_path, conf.network.to_string());

        LampoKeys {
            keys_manager: Arc::new(KeysManager::new(
                &seed,
                start_time.as_secs(),
                start_time.subsec_nanos(),
                PathBuf::from_str(path.as_str()).unwrap(),
            )),
        }
    }

    #[cfg(feature = "vanilla")]
    #[cfg(debug_assertions)]
    pub fn with_channel_keys(seed: [u8; 32], channels_keys: String) -> Self {
        // Fill in random_32_bytes with secure random data, or, on restart, reload the seed from disk.
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        let keys = channels_keys.split('/').collect::<Vec<_>>();

        let mut manager = KeysManager::new(&seed, start_time.as_secs(), start_time.subsec_nanos());
        manager.set_channels_keys(
            keys[1].to_string(),
            keys[2].to_string(),
            keys[3].to_string(),
            keys[4].to_string(),
            keys[5].to_string(),
            keys[6].to_string(),
        );
        LampoKeys {
            keys_manager: Arc::new(manager),
        }
    }

    pub fn inner(&self) -> Arc<KeysManager> {
        self.keys_manager.clone()
    }
}
