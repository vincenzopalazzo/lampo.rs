//! Synchronous bridge to the async VSS client.
use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::thread;

use lampo_common::error;
use lampo_common::ldk::io;
use vss_client::client::VssClient as AsyncVssClient;
use vss_client::error::VssError;
use vss_client::types::{GetObjectRequest, KeyValue, ListKeyVersionsRequest, PutObjectRequest};
use vss_client::util::retry::{ExponentialBackoffRetryPolicy, MaxAttemptsRetryPolicy, RetryPolicy};

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

impl VssClient {
    pub fn new(base_url: &str, store_id: &str) -> error::Result<Self> {
        let (requests, rx) = std_mpsc::channel::<Request>();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let (base_url, store_id) = (base_url.to_owned(), store_id.to_owned());

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
                let client = AsyncVssClient::new(
                    base_url,
                    ExponentialBackoffRetryPolicy::new(std::time::Duration::from_millis(100))
                        .with_max_attempts(3),
                );
                runtime.block_on(run(client, store_id, rx, ready_tx));
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
    let mut versions = snapshot.keys.into_iter().collect::<HashMap<_, _>>();
    let mut global_version = snapshot.global_version;
    let mut poisoned: Option<String> = None;
    if ready.send(Ok(())).is_err() {
        return;
    }

    while let Ok(request) = rx.recv() {
        match request {
            Request::Read { key, reply } => {
                let result = client
                    .get_object(&GetObjectRequest {
                        store_id: store_id.clone(),
                        key,
                    })
                    .await
                    .map_err(|err| match err {
                        VssError::NoSuchKeyError(_) => ReadError::NotFound,
                        other => ReadError::Other(other.to_string()),
                    })
                    .and_then(|response| {
                        response
                            .value
                            .map(|value| value.value)
                            .ok_or_else(|| ReadError::Other("VSS returned no value".to_owned()))
                    });
                let _ = reply.send(result);
            }
            Request::Write { key, value, reply } => {
                if let Some(err) = poisoned.as_ref() {
                    let _ = reply.send(Err(err.clone()));
                    continue;
                }
                let version = versions.get(&key).copied().unwrap_or(0);
                let result = match client
                    .put_object(&PutObjectRequest {
                        store_id: store_id.clone(),
                        global_version: Some(global_version),
                        transaction_items: vec![KeyValue {
                            key: key.clone(),
                            version,
                            value,
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
                    Some(version) => match client
                        .put_object(&PutObjectRequest {
                            store_id: store_id.clone(),
                            global_version: Some(global_version),
                            transaction_items: vec![],
                            delete_items: vec![KeyValue {
                                key: key.clone(),
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
                    },
                };
                let _ = reply.send(result);
            }
            Request::List { prefix, reply } => {
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
