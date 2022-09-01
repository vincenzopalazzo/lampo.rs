//! Persistence module implementation for lampo
//!
//! N.B: This is an experimental version of the persistence,
//! please do not use it in production you can lost funds, or
//! in others words you WILL lost funds, do not trush me!
use lightning_persister::FilesystemPersister;

/// Lampo Persistence implementation.
// FIME: it is a simple wrapper around the ldk file persister
// giving more time to understand how to make a custom one without
// lost funds :-P
pub struct LampoPersistence {
    pub(crate) persister: FilesystemPersister,
    pub(crate) path: String,
}

impl LampoPersistence {
    fn new(path: &str) -> Self {
        return LampoPersistence {
            persister: FilesystemPersister::new(path.to_string()),
            path: path.to_string(),
        };
    }
}

impl Clone for LampoPersistence {
    fn clone(&self) -> Self {
        LampoPersistence {
            path: self.path.to_string(),
            persister: FilesystemPersister::new(self.path.to_string()),
        }
    }
}
