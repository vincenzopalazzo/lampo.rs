use lightning::chain::keysinterface::KeysManager;
use std::time::SystemTime;

/// Lampo keys implementations
pub struct LampoKeys {
    pub(crate) keys_manager: KeysManager,
}

impl LampoKeys {
    fn new() -> LampoKeys {
        // FIXME: use some standard derivation to make the wallet recoverable
        let mut random_32_bytes = [0; 32];
        // Fill in random_32_bytes with secure random data, or, on restart, reload the seed from disk.
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        LampoKeys {
            // FIXME: store this seeds somewhere!
            keys_manager: KeysManager::new(
                &random_32_bytes,
                start_time.as_secs(),
                start_time.subsec_nanos(),
            ),
        }
    }
}
