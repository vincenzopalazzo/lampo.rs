//! Node-side signer state that VLS does not keep for us.
//!
//! `next_dbid` must survive restarts: the signer raises a high-water mark
//! when a channel is forgotten, and reusing a lower dbid is a policy error.
//! The receive-auth key is LDK's blinded-path MAC key; the signer does not
//! provide it and LDK only needs it to be stable.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lampo_common::json;

pub struct SignerState {
    path: PathBuf,
    pub next_dbid: u64,
    pub receive_auth_key: [u8; 32],
}

impl SignerState {
    /// Load the state file, or create it with a fresh auth key from `entropy`.
    pub fn load_or_create(path: &Path, entropy: impl FnOnce() -> [u8; 32]) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Self::parse(path.to_path_buf(), &bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let state = Self {
                    path: path.to_path_buf(),
                    next_dbid: 1,
                    receive_auth_key: entropy(),
                };
                state.save()?;
                Ok(state)
            }
            Err(err) => Err(err),
        }
    }

    fn parse(path: PathBuf, bytes: &[u8]) -> io::Result<Self> {
        let value: json::Value = json::from_slice(bytes).map_err(invalid)?;
        let next_dbid = value["next_dbid"]
            .as_u64()
            .ok_or_else(|| invalid("missing next_dbid"))?;
        let key = value["receive_auth_key"]
            .as_str()
            .ok_or_else(|| invalid("missing receive_auth_key"))?;
        let key = hex::decode(key).map_err(invalid)?;
        let receive_auth_key: [u8; 32] = key
            .try_into()
            .map_err(|_| invalid("receive_auth_key is not 32 bytes"))?;
        Ok(Self {
            path,
            next_dbid,
            receive_auth_key,
        })
    }

    /// Reserve the next dbid and persist the counter before handing it out,
    /// so a crash between the two cannot reuse it.
    pub fn take_dbid(&mut self) -> io::Result<u64> {
        let dbid = self.next_dbid;
        self.next_dbid += 1;
        self.save()?;
        Ok(dbid)
    }

    fn save(&self) -> io::Result<()> {
        let value = json::json!({
            "next_dbid": self.next_dbid,
            "receive_auth_key": hex::encode(self.receive_auth_key),
        });
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json::to_vec_pretty(&value).map_err(invalid)?)?;
        fs::rename(&tmp, &self.path)
    }
}

fn invalid(err: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::SignerState;

    #[test]
    fn state_persists_the_dbid_counter_and_auth_key() {
        let dir = std::env::temp_dir().join(format!("lampo-vls-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vls-signer.json");

        let mut state = SignerState::load_or_create(&path, || [7u8; 32]).unwrap();
        assert_eq!(state.take_dbid().unwrap(), 1);
        assert_eq!(state.take_dbid().unwrap(), 2);

        let reloaded =
            SignerState::load_or_create(&path, || panic!("must not regenerate")).unwrap();
        assert_eq!(reloaded.next_dbid, 3);
        assert_eq!(reloaded.receive_auth_key, [7u8; 32]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
