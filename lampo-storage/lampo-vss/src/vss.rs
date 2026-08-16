//! The [`ShadowSink`] that talks to a VSS server.
//!
//! Thin on purpose: the queueing, retry and lag reporting all live in the
//! shadow, so this only has to put or delete one value and say whether it
//! worked.
use std::sync::mpsc as std_mpsc;
use std::thread;

use lampo_common::error;
use vss_client::client::VssClient;
use vss_client::error::VssError;
use vss_client::types::{KeyValue, PutObjectRequest};
use vss_client::util::retry::{ExponentialBackoffRetryPolicy, RetryPolicy};

use crate::ShadowSink;

/// One mutation to apply, and where the answer goes.
enum Request {
    Put {
        key: String,
        value: Vec<u8>,
        reply: std_mpsc::SyncSender<Result<(), String>>,
    },
    Delete {
        key: String,
        reply: std_mpsc::SyncSender<Result<(), String>>,
    },
}

/// A VSS server holding the shadow copy.
///
/// The client is async and [`ShadowSink`] is not, so, as with the Postgres
/// backend, the sink owns a thread with its own runtime and is spoken to over a
/// channel. Nothing here nests one runtime inside another.
pub struct VssSink {
    requests: std_mpsc::Sender<Request>,
}

impl VssSink {
    /// Store values for `store_id` at `base_url`.
    pub fn new(base_url: &str, store_id: &str) -> error::Result<Self> {
        let (requests, rx) = std_mpsc::channel::<Request>();
        let (base_url, store_id) = (base_url.to_owned(), store_id.to_owned());

        thread::Builder::new()
            .name("lampo-vss-sink".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        log::error!(target: "lampo-vss", "no runtime for the VSS sink: {err}");
                        return;
                    }
                };
                // The client retries transient failures itself; the shadow's
                // own backoff is the outer, slower loop for a server that is
                // down rather than merely busy.
                let client = VssClient::new(
                    base_url,
                    ExponentialBackoffRetryPolicy::new(std::time::Duration::from_millis(100))
                        .with_max_attempts(3),
                );

                runtime.block_on(async move {
                    while let Ok(request) = rx.recv() {
                        let (reply, result) = match request {
                            Request::Put { key, value, reply } => {
                                let request = PutObjectRequest {
                                    store_id: store_id.clone(),
                                    global_version: None,
                                    transaction_items: vec![KeyValue {
                                        key,
                                        // Versioning is the server's concurrency
                                        // control. Lampo has a single writer, so the
                                        // copy always takes the newest value.
                                        version: -1,
                                        value,
                                    }],
                                    delete_items: vec![],
                                };
                                (
                                    reply,
                                    client
                                        .put_object(&request)
                                        .await
                                        .map(|_| ())
                                        .map_err(|err| format!("{err:?}")),
                                )
                            }
                            Request::Delete { key, reply } => {
                                let request = PutObjectRequest {
                                    store_id: store_id.clone(),
                                    global_version: None,
                                    transaction_items: vec![],
                                    delete_items: vec![KeyValue {
                                        key,
                                        version: -1,
                                        value: Vec::new(),
                                    }],
                                };
                                let result = match client.put_object(&request).await {
                                    Ok(_) => Ok(()),
                                    // Desired end state is "absent". VSS reports a
                                    // missing key as conflict/no-such-key.
                                    Err(VssError::ConflictError(_))
                                    | Err(VssError::NoSuchKeyError(_)) => Ok(()),
                                    Err(err) => Err(format!("{err:?}")),
                                };
                                (reply, result)
                            }
                        };
                        let _ = reply.send(result);
                    }
                });
            })?;

        Ok(Self { requests })
    }
}

impl ShadowSink for VssSink {
    fn put(&self, key: &str, value: &[u8]) -> error::Result<()> {
        let (reply, answer) = std_mpsc::sync_channel(1);
        self.requests
            .send(Request::Put {
                key: key.to_owned(),
                value: value.to_vec(),
                reply,
            })
            .map_err(|_| error::anyhow!("the VSS sink thread is gone"))?;
        answer
            .recv()
            .map_err(|_| error::anyhow!("the VSS sink thread dropped the reply"))?
            .map_err(|err| error::anyhow!("{err}"))
    }

    fn delete(&self, key: &str) -> error::Result<()> {
        let (reply, answer) = std_mpsc::sync_channel(1);
        self.requests
            .send(Request::Delete {
                key: key.to_owned(),
                reply,
            })
            .map_err(|_| error::anyhow!("the VSS sink thread is gone"))?;
        answer
            .recv()
            .map_err(|_| error::anyhow!("the VSS sink thread dropped the reply"))?
            .map_err(|err| error::anyhow!("{err}"))
    }
}
