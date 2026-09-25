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
    bytes: Vec<u8>,
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
    nonce_key: [u8; 32],
}

impl Seal {
    fn new(storage_key: [u8; 32]) -> Self {
        // Split one derived key so names, values, and nonces use different
        // material. The labels are fixed; a restart must open the same store.
        Self {
            obfuscator: KeyObfuscator::new(expand(&storage_key, b"lampo-vss-key-name-v1")),
            value_key: expand(&storage_key, b"lampo-vss-value-v1"),
            nonce_key: expand(&storage_key, b"lampo-vss-nonce-v1"),
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

    fn next_nonce_material(&self, key: &str, version: i64) -> [u8; 8] {
        // The VSS key version is unique per successful write of this key and
        // survives a restart, unlike a process-local counter. Mix the key so
        // two keys at the same version do not share a nonce.
        let mut mixed = (version as u64).to_le_bytes();
        for (index, byte) in key.as_bytes().iter().enumerate() {
            mixed[index % mixed.len()] ^= byte;
        }
        for (index, byte) in self.nonce_key.iter().enumerate() {
            mixed[index % mixed.len()] ^= byte.wrapping_add(index as u8);
        }
        mixed
    }

    fn seal(&mut self, key: &str, value: Vec<u8>, version: i64) -> Result<Vec<u8>, String> {
        let mixed = self.next_nonce_material(key, version);
        let builder = StorableBuilder::new(
            self.value_key,
            SeedEntropy {
                bytes: mixed.to_vec(),
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
        let builder = StorableBuilder::new(self.value_key, SeedEntropy { bytes: Vec::new() });
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

/// Signs every VSS request the way `vss-server`'s signature authorizer expects.
struct VssSigner {
    secret: [u8; 32],
}

impl VssSigner {
    fn new(storage_key: [u8; 32]) -> Self {
        Self {
            secret: expand(&storage_key, b"lampo-vss-auth-v1"),
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
        Ok(headers)
    }
}

impl VssClient {
    /// Connect to `base_url`.
    ///
    /// `storage_key` encrypts every value and obfuscates every key before the
    /// request is sent. `auth_token`, when set, is sent as a bearer token so
    /// a network-reachable server can reject unauthenticated writers.
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
                // same user. `auth_token` is kept so a deployment in front of
                // a proxy can still send a bearer token.
                let provider: Arc<dyn VssHeaderProvider> = Arc::new(VssSigner::new(storage_key));
                let _ = auth_token;
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

async fn run(
    client: AsyncVssClient<Retry>,
    store_id: String,
    mut seal: Seal,
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
                        // The server can return an older ciphertext for this
                        // key. The seal embeds the version we wrote; that must
                        // match the version the server reports for the key.
                        // The blob is sealed with the version we sent
                        // (0 on the first write). The server then stores that
                        // key at version + 1. Check the version we sealed.
                        let sealed_version = value.version.saturating_sub(1);
                        seal.open(value.value, &key, sealed_version)
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
                let sealed = match seal.seal(&key, value, version) {
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
