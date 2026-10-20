//! Synchronous bridge to the async VSS client.
use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;

use lampo_common::error;
use lampo_common::ldk::io;
use prost::Message;
use vss_client::client::VssClient as AsyncVssClient;
use vss_client::error::VssError;
use vss_client::headers::VssHeaderProvider;
use vss_client::types::{GetObjectRequest, KeyValue, ListKeyVersionsRequest, PutObjectRequest};
use vss_client::util::key_obfuscator::KeyObfuscator;
use vss_client::util::retry::{ExponentialBackoffRetryPolicy, MaxAttemptsRetryPolicy, RetryPolicy};
use vss_client::util::storable_builder::{EntropySource, StorableBuilder};

type Retry = MaxAttemptsRetryPolicy<ExponentialBackoffRetryPolicy<VssError>>;

enum Request {
    Read {
        key: String,
        reply: std_mpsc::SyncSender<Result<Vec<u8>, ReadError>>,
    },
    Write {
        key: String,
        value: Vec<u8>,
        reply: std_mpsc::SyncSender<Result<(), String>>,
    },
    Remove {
        key: String,
        reply: std_mpsc::SyncSender<Result<(), String>>,
    },
    List {
        prefix: String,
        reply: std_mpsc::SyncSender<Result<Vec<String>, String>>,
    },
}

enum ReadError {
    NotFound,
    Other(String),
}

/// A blocking VSS client whose worker serializes mutations and tracks versions.
pub struct VssClient {
    requests: std_mpsc::Sender<Request>,
}

struct SeedEntropy {
    bytes: [u8; 8],
}

impl EntropySource for SeedEntropy {
    fn fill_bytes(&self, buffer: &mut [u8]) {
        // StorableBuilder asks for exactly 8 nonce bytes (nonce[4..]). A
        // mismatch must not silently fall back to a zero nonce.
        assert_eq!(
            self.bytes.len(),
            buffer.len(),
            "VSS nonce material must match the builder's request"
        );
        buffer.copy_from_slice(&self.bytes);
    }
}

fn expand(key: &[u8; 32], label: &[u8]) -> [u8; 32] {
    use lampo_common::bitcoin::hashes::{sha256, Hash, HashEngine, Hmac, HmacEngine};

    let mut engine = HmacEngine::<sha256::Hash>::new(key);
    engine.input(label);
    Hmac::<sha256::Hash>::from_engine(engine).to_byte_array()
}

/// Encrypt values and obfuscate keys before they leave the process.
struct Seal {
    obfuscator: KeyObfuscator,
    value_key: [u8; 32],
}

impl Seal {
    fn new(storage_key: [u8; 32]) -> Self {
        Self {
            obfuscator: KeyObfuscator::new(expand(&storage_key, b"lampo-vss-key-name-v1")),
            value_key: expand(&storage_key, b"lampo-vss-value-v1"),
        }
    }

    fn hide_key(&self, key: &str) -> String {
        self.obfuscator.obfuscate(key)
    }

    fn show_key(&self, key: &str) -> Result<String, String> {
        self.obfuscator
            .deobfuscate(key)
            .map_err(|err| format!("deobfuscating VSS key: {err}"))
    }

    fn seal(&self, value: Vec<u8>, version: i64) -> Result<Vec<u8>, String> {
        use lampo_common::bitcoin::secp256k1::rand::{rngs::OsRng, RngCore};

        // The nonce is stored in the Storable encryption metadata, so fresh
        // CSPRNG output remains decryptable and prevents concurrent writers
        // sealing the same key/version from reusing a nonce.
        let mut nonce_material = [0u8; 8];
        OsRng.fill_bytes(&mut nonce_material);
        let builder = StorableBuilder::new(
            self.value_key,
            SeedEntropy {
                bytes: nonce_material,
            },
        );
        let storable = builder.build(value, version);
        let mut encoded = Vec::new();
        storable
            .encode(&mut encoded)
            .map_err(|err| format!("encoding encrypted VSS value: {err}"))?;
        Ok(encoded)
    }

    fn open(&self, encoded: Vec<u8>, key: &str, expected_version: i64) -> Result<Vec<u8>, String> {
        let storable = vss_client::types::Storable::decode(encoded.as_slice())
            .map_err(|err| format!("decoding encrypted VSS value: {err}"))?;
        let builder = StorableBuilder::new(self.value_key, SeedEntropy { bytes: [0; 8] });
        let (value, embedded) = builder
            .deconstruct(storable)
            .map_err(|err| format!("decrypting VSS value: {err}"))?;
        // The server can replay an older ciphertext under the current key.
        // The version inside the seal must be the version we asked for.
        if embedded != expected_version {
            return Err(format!(
                "VSS value for `{key}` sealed version {embedded}, expected {expected_version}"
            ));
        }
        Ok(value)
    }
}

const VSS_SIGNING_CONSTANT: &[u8] =
    b"VSS Signature Authorizer Signing Salt Constant..................";
const VSS_AUTH_TOKEN_HEADER: &str = "X-VSS-Auth-Token";

/// Signs every VSS request the way `vss-server`'s signature authorizer expects.
struct VssSigner {
    secret: [u8; 32],
    auth_token: Option<String>,
}

impl VssSigner {
    fn new(storage_key: [u8; 32], auth_token: Option<String>) -> Self {
        Self {
            secret: expand(&storage_key, b"lampo-vss-auth-v1"),
            auth_token,
        }
    }

    fn authorization_header(&self) -> String {
        use lampo_common::bitcoin::hashes::{sha256, Hash, HashEngine};
        use lampo_common::bitcoin::secp256k1::{Message, Secp256k1, SecretKey};

        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&self.secret).expect("32-byte auth key");
        let pubkey = secret.public_key(&secp);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        let time = now.to_string();
        let mut engine = sha256::Hash::engine();
        engine.input(VSS_SIGNING_CONSTANT);
        engine.input(&pubkey.serialize());
        engine.input(time.as_bytes());
        let digest = sha256::Hash::from_engine(engine);
        let signature = secp.sign_ecdsa(&Message::from_digest(digest.to_byte_array()), &secret);
        format!(
            "{:x}{}{time}",
            pubkey,
            signature
                .serialize_compact()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }
}

#[async_trait::async_trait]
impl VssHeaderProvider for VssSigner {
    async fn get_headers(
        &self,
        _request: &[u8],
    ) -> Result<
        std::collections::HashMap<String, String>,
        vss_client::headers::VssHeaderProviderError,
    > {
        let mut headers = std::collections::HashMap::new();
        headers.insert("Authorization".to_owned(), self.authorization_header());
        if let Some(token) = self.auth_token.as_ref() {
            headers.insert(VSS_AUTH_TOKEN_HEADER.to_owned(), token.clone());
        }
        Ok(headers)
    }
}

impl VssClient {
    /// Connect to `base_url`.
    ///
    /// `storage_key` encrypts every value and obfuscates every key before the
    /// request is sent. `auth_token`, when set, is sent in
    /// `X-VSS-Auth-Token` so it does not replace the VSS signature in
    /// `Authorization`.
    pub fn new(
        base_url: &str,
        store_id: &str,
        storage_key: [u8; 32],
        auth_token: Option<String>,
    ) -> error::Result<Self> {
        let (requests, rx) = std_mpsc::channel::<Request>();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let (base_url, store_id) = (base_url.to_owned(), store_id.to_owned());
        let seal = Seal::new(storage_key);

        thread::Builder::new()
            .name("lampo-vss".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = ready_tx.send(Err(format!("creating VSS runtime: {err}")));
                        return;
                    }
                };
                let retry =
                    ExponentialBackoffRetryPolicy::new(std::time::Duration::from_millis(100))
                        .with_max_attempts(3);
                // vss-server grants access to the pubkey that signs each
                // request. A static bearer token is not that proof. Sign with
                // a key derived from the storage key so a restart opens the
                // same user. A proxy token uses its own header because the
                // signature authorizer owns `Authorization`.
                let provider: Arc<dyn VssHeaderProvider> =
                    Arc::new(VssSigner::new(storage_key, auth_token));
                let client = AsyncVssClient::new_with_headers(base_url, retry, provider);
                runtime.block_on(run(client, store_id, seal, rx, ready_tx));
            })?;

        ready_rx
            .recv()
            .map_err(|_| error::anyhow!("VSS worker exited during startup"))?
            .map_err(|err| error::anyhow!("{err}"))?;
        Ok(Self { requests })
    }

    pub fn read(&self, key: &str) -> Result<Vec<u8>, io::Error> {
        let (reply, answer) = std_mpsc::sync_channel(1);
        self.requests
            .send(Request::Read {
                key: key.to_owned(),
                reply,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "VSS worker is gone"))?;
        match answer.recv() {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(ReadError::NotFound)) => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "VSS key does not exist",
            )),
            Ok(Err(ReadError::Other(err))) => Err(io::Error::new(io::ErrorKind::Other, err)),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::Other,
                "VSS worker dropped the reply",
            )),
        }
    }

    pub fn write(&self, key: &str, value: Vec<u8>) -> error::Result<()> {
        self.request(|reply| Request::Write {
            key: key.to_owned(),
            value,
            reply,
        })
    }

    pub fn remove(&self, key: &str) -> error::Result<()> {
        self.request(|reply| Request::Remove {
            key: key.to_owned(),
            reply,
        })
    }

    pub fn list(&self, prefix: &str) -> error::Result<Vec<String>> {
        let (reply, answer) = std_mpsc::sync_channel(1);
        self.requests
            .send(Request::List {
                prefix: prefix.to_owned(),
                reply,
            })
            .map_err(|_| error::anyhow!("VSS worker is gone"))?;
        answer
            .recv()
            .map_err(|_| error::anyhow!("VSS worker dropped the reply"))?
            .map_err(|err| error::anyhow!("{err}"))
    }

    fn request(
        &self,
        request: impl FnOnce(std_mpsc::SyncSender<Result<(), String>>) -> Request,
    ) -> error::Result<()> {
        let (reply, answer) = std_mpsc::sync_channel(1);
        self.requests
            .send(request(reply))
            .map_err(|_| error::anyhow!("VSS worker is gone"))?;
        answer
            .recv()
            .map_err(|_| error::anyhow!("VSS worker dropped the reply"))?
            .map_err(|err| error::anyhow!("{err}"))
    }
}

fn open_at_cached_version(
    seal: &Seal,
    key: &str,
    cached_version: Option<i64>,
    value: KeyValue,
) -> Result<Vec<u8>, String> {
    let expected_version = cached_version.ok_or_else(|| {
        format!("VSS returned `{key}` although it was absent from the startup inventory")
    })?;
    if value.version != expected_version {
        return Err(format!(
            "VSS rollback detected for `{key}`: response version {}, cached version {expected_version}",
            value.version
        ));
    }

    // A put is sealed with the version supplied by the client. The server
    // stores that write at the next key version.
    seal.open(value.value, key, expected_version.saturating_sub(1))
}

async fn run(
    client: AsyncVssClient<Retry>,
    store_id: String,
    seal: Seal,
    rx: std_mpsc::Receiver<Request>,
    ready: std_mpsc::SyncSender<Result<(), String>>,
) {
    let snapshot = match list_key_versions(&client, &store_id, None).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    // The server only ever sees obfuscated keys. Translate them back before
    // the rest of the node looks up a channel-manager key by its plain name.
    let mut versions = HashMap::new();
    for (remote_key, version) in snapshot.keys {
        match seal.show_key(&remote_key) {
            Ok(key) => {
                versions.insert(key, version);
            }
            Err(err) => {
                log::error!(target: "lampo-vss", "skipping unrecognized VSS key: {err}");
            }
        }
    }
    let mut global_version = snapshot.global_version;
    let mut poisoned: Option<String> = None;
    if ready.send(Ok(())).is_err() {
        return;
    }

    while let Ok(request) = rx.recv() {
        match request {
            Request::Read { key, reply } => {
                let remote_key = seal.hide_key(&key);
                let result = client
                    .get_object(&GetObjectRequest {
                        store_id: store_id.clone(),
                        key: remote_key,
                    })
                    .await
                    .map_err(|err| match err {
                        VssError::NoSuchKeyError(_) => ReadError::NotFound,
                        other => ReadError::Other(other.to_string()),
                    })
                    .and_then(|response| {
                        response
                            .value
                            .ok_or_else(|| ReadError::Other("VSS returned no value".to_owned()))
                    })
                    .and_then(|value| {
                        open_at_cached_version(&seal, &key, versions.get(&key).copied(), value)
                            .map_err(ReadError::Other)
                    });
                let _ = reply.send(result);
            }
            Request::Write { key, value, reply } => {
                if let Some(err) = poisoned.as_ref() {
                    let _ = reply.send(Err(err.clone()));
                    continue;
                }
                let version = versions.get(&key).copied().unwrap_or(0);
                let sealed = match seal.seal(value, version) {
                    Ok(sealed) => sealed,
                    Err(err) => {
                        let _ = reply.send(Err(err));
                        continue;
                    }
                };
                let remote_key = seal.hide_key(&key);
                let result = match client
                    .put_object(&PutObjectRequest {
                        store_id: store_id.clone(),
                        global_version: Some(global_version),
                        transaction_items: vec![KeyValue {
                            key: remote_key,
                            version,
                            value: sealed,
                        }],
                        delete_items: vec![],
                    })
                    .await
                {
                    Ok(_) => {
                        versions.insert(key, version + 1);
                        global_version += 1;
                        Ok(())
                    }
                    Err(err) => {
                        let message = describe_write_error(err);
                        if is_conflict(&message) {
                            poisoned = Some(message.clone());
                        }
                        Err(message)
                    }
                };
                let _ = reply.send(result);
            }
            Request::Remove { key, reply } => {
                if let Some(err) = poisoned.as_ref() {
                    let _ = reply.send(Err(err.clone()));
                    continue;
                }
                let result = match versions.get(&key).copied() {
                    None => Ok(()),
                    Some(version) => {
                        let remote_key = seal.hide_key(&key);
                        match client
                            .put_object(&PutObjectRequest {
                                store_id: store_id.clone(),
                                global_version: Some(global_version),
                                transaction_items: vec![],
                                delete_items: vec![KeyValue {
                                    key: remote_key,
                                    version,
                                    value: vec![],
                                }],
                            })
                            .await
                        {
                            Ok(_) => {
                                versions.remove(&key);
                                global_version += 1;
                                Ok(())
                            }
                            Err(err) => {
                                let message = describe_write_error(err);
                                if is_conflict(&message) {
                                    poisoned = Some(message.clone());
                                }
                                Err(message)
                            }
                        }
                    }
                };
                let _ = reply.send(result);
            }
            Request::List { prefix, reply } => {
                // Prefixes are plaintext. The version map is too, so a list
                // never has to ask the server to filter on a ciphertext key.
                let mut keys: Vec<_> = versions
                    .keys()
                    .filter(|key| key.starts_with(&prefix))
                    .cloned()
                    .collect();
                keys.sort();
                let _ = reply.send(Ok(keys));
            }
        }
    }
}

fn describe_write_error(err: VssError) -> String {
    match err {
        VssError::ConflictError(message) => {
            format!("VSS writer conflict: another lampod may be using this store ({message})")
        }
        other => other.to_string(),
    }
}

fn is_conflict(message: &str) -> bool {
    message.starts_with("VSS writer conflict:")
}

async fn list_key_versions(
    client: &AsyncVssClient<Retry>,
    store_id: &str,
    prefix: Option<String>,
) -> Result<StoreSnapshot, String> {
    let mut versions = Vec::new();
    let mut page_token = None;
    let mut global_version = None;
    loop {
        let response = client
            .list_key_versions(&ListKeyVersionsRequest {
                store_id: store_id.to_owned(),
                key_prefix: prefix.clone(),
                page_size: None,
                page_token,
            })
            .await
            .map_err(|err| err.to_string())?;
        if global_version.is_none() {
            global_version = response.global_version;
        }
        versions.extend(
            response
                .key_versions
                .into_iter()
                .map(|item| (item.key, item.version)),
        );
        match response.next_page_token {
            Some(token) if !token.is_empty() => page_token = Some(token),
            _ => break,
        }
    }
    Ok(StoreSnapshot {
        keys: versions,
        global_version: global_version.unwrap_or(0),
    })
}

struct StoreSnapshot {
    keys: Vec<(String, i64)>,
    global_version: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_token_does_not_replace_signature_authorization() {
        let signer = VssSigner::new([7u8; 32], Some("proxy-secret".to_owned()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let headers = runtime.block_on(signer.get_headers(&[])).unwrap();

        assert_eq!(
            headers.get(VSS_AUTH_TOKEN_HEADER).map(String::as_str),
            Some("proxy-secret")
        );
        let signature = headers.get("Authorization").unwrap();
        assert!(!signature.is_empty());
        assert_ne!(signature, "proxy-secret");
    }

    #[test]
    fn sealing_same_version_uses_unique_random_nonces() {
        let seal = Seal::new([3u8; 32]);
        let first = seal.seal(b"value".to_vec(), 7).unwrap();
        let second = seal.seal(b"value".to_vec(), 7).unwrap();
        let first_storable = vss_client::types::Storable::decode(first.as_slice()).unwrap();
        let second_storable = vss_client::types::Storable::decode(second.as_slice()).unwrap();

        assert_ne!(
            first_storable.encryption_metadata.unwrap().nonce,
            second_storable.encryption_metadata.unwrap().nonce
        );
        assert_eq!(
            seal.open(second, "key", 7).unwrap(),
            b"value",
            "the random nonce must remain encoded for decryption"
        );
    }

    #[test]
    fn read_rejects_response_older_than_inventory() {
        let seal = Seal::new([5u8; 32]);
        let encrypted = seal.seal(b"old".to_vec(), 2).unwrap();
        let err = open_at_cached_version(
            &seal,
            "manager",
            Some(4),
            KeyValue {
                key: seal.hide_key("manager"),
                version: 3,
                value: encrypted,
            },
        )
        .unwrap_err();

        assert!(err.contains("rollback detected"), "unexpected error: {err}");
        assert!(
            err.contains("response version 3"),
            "unexpected error: {err}"
        );
        assert!(err.contains("cached version 4"), "unexpected error: {err}");
    }

    #[test]
    fn read_accepts_response_matching_inventory_and_seal() {
        let seal = Seal::new([5u8; 32]);
        let encrypted = seal.seal(b"current".to_vec(), 3).unwrap();
        let value = open_at_cached_version(
            &seal,
            "manager",
            Some(4),
            KeyValue {
                key: seal.hide_key("manager"),
                version: 4,
                value: encrypted,
            },
        )
        .unwrap();

        assert_eq!(value, b"current");
    }
}
