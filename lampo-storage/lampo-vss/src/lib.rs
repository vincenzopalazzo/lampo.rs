//! VSS persistence backend.
//!
//! VSS is a primary store, not a best-effort shadow. Every operation waits for
//! the server, and a write is acknowledged only after VSS commits it.
use std::collections::HashMap;
use std::sync::Mutex;

use lampo_common::error;
use lampo_common::json;
use lampo_common::ldk::io;
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::persist::{
    LampoPersistenceBackend, PaymentFilter, PaymentRecord, PaymentStore, PersistenceKind,
    PAYMENTS_NAMESPACE,
};
use url::{Host, Url};

mod vss;

use vss::VssClient;

/// VSS-backed Lampo persistence.
pub struct VssStore {
    client: Box<dyn StoreClient>,
    /// `None` until the first payment read. Startup must not download every
    /// payment blob before the node can listen.
    payments: Mutex<Option<HashMap<String, PaymentRecord>>>,
}

trait StoreClient: Send + Sync {
    fn read(&self, key: &str) -> Result<Vec<u8>, io::Error>;
    fn write(&self, key: &str, value: Vec<u8>) -> error::Result<()>;
    fn remove(&self, key: &str) -> error::Result<()>;
    fn list(&self, prefix: &str) -> error::Result<Vec<String>>;
}

impl StoreClient for VssClient {
    fn read(&self, key: &str) -> Result<Vec<u8>, io::Error> {
        VssClient::read(self, key)
    }

    fn write(&self, key: &str, value: Vec<u8>) -> error::Result<()> {
        VssClient::write(self, key, value)
    }

    fn remove(&self, key: &str) -> error::Result<()> {
        VssClient::remove(self, key)
    }

    fn list(&self, prefix: &str) -> error::Result<Vec<String>> {
        VssClient::list(self, prefix)
    }
}

impl VssStore {
    /// Connect to `base_url` and use `store_id` as the node's isolated keyspace.
    ///
    /// `storage_key` encrypts values and obfuscates keys before they are
    /// uploaded. `auth_token` is sent in `X-VSS-Auth-Token` when the server
    /// requires one; a remote URL without a token is refused.
    pub fn new(
        base_url: &str,
        store_id: &str,
        storage_key: [u8; 32],
        auth_token: Option<String>,
        global_version_path: std::path::PathBuf,
    ) -> error::Result<Self> {
        let is_loopback = validate_vss_url(base_url)?;
        if !is_loopback && auth_token.as_deref().unwrap_or("").is_empty() {
            error::bail!(
                "storage=vss on a non-local URL needs storage-auth-token;                  the store id is public and is not an authenticator"
            );
        }
        let client: Box<dyn StoreClient> = Box::new(VssClient::new(
            base_url,
            store_id,
            storage_key,
            auth_token,
            global_version_path,
        )?);
        Ok(Self {
            client,
            payments: Mutex::new(None),
        })
    }

    #[cfg(test)]
    fn with_client(client: Box<dyn StoreClient>) -> error::Result<Self> {
        let payments = load_payments(client.as_ref())?;
        Ok(Self {
            client,
            payments: Mutex::new(Some(payments)),
        })
    }

    /// Load payment history on the first read, not while the daemon is starting.
    fn payment_cache(
        &self,
    ) -> error::Result<std::sync::MutexGuard<'_, Option<HashMap<String, PaymentRecord>>>> {
        let mut cache = self
            .payments
            .lock()
            .map_err(|err| error::anyhow!("payment map poisoned: {err}"))?;
        if cache.is_none() {
            *cache = Some(load_payments(self.client.as_ref())?);
        }
        Ok(cache)
    }
}

/// Return a stable, credential-free VSS endpoint for logs and local identity.
///
/// The original URL is still used for requests so deployments using signed
/// query strings keep working. Userinfo, query parameters, and fragments must
/// never be logged or written to `.lampo-storage-backend`.
pub fn canonical_vss_url(base_url: &str) -> error::Result<String> {
    let mut url = parse_vss_url(base_url)?;
    url.set_username("")
        .map_err(|_| error::anyhow!("storage-url cannot contain this username"))?;
    url.set_password(None)
        .map_err(|_| error::anyhow!("storage-url cannot contain this password"))?;
    url.set_query(None);
    url.set_fragment(None);

    let mut canonical = url.to_string();
    while canonical.ends_with('/') {
        canonical.pop();
    }
    Ok(canonical)
}

fn parse_vss_url(base_url: &str) -> error::Result<Url> {
    let url = Url::parse(base_url).map_err(|err| error::anyhow!("invalid storage-url: {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        error::bail!("storage-url must use http or https");
    }
    if url.host_str().is_none() {
        error::bail!("storage-url must include a host");
    }
    Ok(url)
}

fn validate_vss_url(base_url: &str) -> error::Result<bool> {
    let url = parse_vss_url(base_url)?;
    let is_loopback = match url.host().expect("parse_vss_url requires a host") {
        Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    };
    if url.scheme() == "http" && !is_loopback {
        error::bail!("storage-url must use https for a non-loopback host");
    }
    Ok(is_loopback)
}

fn storage_key(primary_namespace: &str, secondary_namespace: &str, key: &str) -> String {
    format!("{primary_namespace}/{secondary_namespace}/{key}")
}

fn namespace_prefix(primary_namespace: &str, secondary_namespace: &str) -> String {
    format!("{primary_namespace}/{secondary_namespace}/")
}

fn split_storage_key(key: &str) -> error::Result<(String, String, String)> {
    let mut parts = key.splitn(3, '/');
    let primary = parts
        .next()
        .ok_or_else(|| error::anyhow!("VSS key has no primary namespace"))?;
    let secondary = parts
        .next()
        .ok_or_else(|| error::anyhow!("VSS key has no secondary namespace"))?;
    let key = parts
        .next()
        .ok_or_else(|| error::anyhow!("VSS key has no key component"))?;
    Ok((primary.to_owned(), secondary.to_owned(), key.to_owned()))
}

fn load_payments(client: &dyn StoreClient) -> error::Result<HashMap<String, PaymentRecord>> {
    let mut payments = HashMap::new();
    for key in client.list(&namespace_prefix(PAYMENTS_NAMESPACE, ""))? {
        // Payment history is bookkeeping. A corrupt or old-schema row must
        // not keep a node with intact channel state from starting. The
        // filesystem backend already skips these.
        let encoded = match client.read(&key) {
            Ok(encoded) => encoded,
            Err(err) => {
                log::error!(target: "lampo-vss", "skipping VSS payment `{key}`: {err}");
                continue;
            }
        };
        match json::from_slice::<PaymentRecord>(&encoded) {
            Ok(payment) => {
                payments.insert(payment.id.clone(), payment);
            }
            Err(err) => {
                log::error!(target: "lampo-vss", "skipping VSS payment `{key}`: {err}");
            }
        }
    }
    Ok(payments)
}

fn io_err(err: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::Other, err.to_string())
}

impl KVStoreSync for VssStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        self.client
            .read(&storage_key(primary_namespace, secondary_namespace, key))
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        self.client
            .write(
                &storage_key(primary_namespace, secondary_namespace, key),
                buf,
            )
            .map_err(io_err)
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Result<(), io::Error> {
        self.client
            .remove(&storage_key(primary_namespace, secondary_namespace, key))
            .map_err(io_err)
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        let prefix = namespace_prefix(primary_namespace, secondary_namespace);
        self.client
            .list(&prefix)
            .map(|keys| {
                keys.into_iter()
                    .filter_map(|key| key.strip_prefix(&prefix).map(str::to_owned))
                    .collect()
            })
            .map_err(io_err)
    }
}

impl PaymentStore for VssStore {
    fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()> {
        let mut cache = self.payment_cache()?;
        let payments = cache
            .as_mut()
            .expect("payment cache is loaded before an upsert");

        self.client
            .write(
                &storage_key(PAYMENTS_NAMESPACE, "", &payment.id),
                json::to_vec(payment)?,
            )
            .map_err(|err| error::anyhow!("writing VSS payment `{}`: {err}", payment.id))?;
        payments.insert(payment.id.clone(), payment.clone());
        Ok(())
    }

    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>> {
        Ok(self
            .payment_cache()?
            .as_ref()
            .and_then(|payments| payments.get(id).cloned()))
    }

    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
        let cache = self.payment_cache()?;
        let payments = cache
            .as_ref()
            .expect("payment cache is loaded before a list");
        let mut matched: Vec<_> = payments
            .values()
            .filter(|record| filter.matches(record))
            .cloned()
            .collect();
        matched.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(filter.paginate(matched))
    }
}

impl LampoPersistenceBackend for VssStore {
    fn kind(&self) -> PersistenceKind {
        PersistenceKind::Vss
    }

    fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>> {
        self.client.list("").and_then(|keys| {
            keys.into_iter()
                .map(|key| split_storage_key(&key))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lampo_common::persist::{PaymentDirection, PaymentStatus};

    use super::*;

    #[derive(Clone, Default)]
    struct FakeClient {
        values: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    impl StoreClient for FakeClient {
        fn read(&self, key: &str) -> Result<Vec<u8>, io::Error> {
            self.values
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing key"))
        }

        fn write(&self, key: &str, value: Vec<u8>) -> error::Result<()> {
            self.values.lock().unwrap().insert(key.to_owned(), value);
            Ok(())
        }

        fn remove(&self, key: &str) -> error::Result<()> {
            self.values.lock().unwrap().remove(key);
            Ok(())
        }

        fn list(&self, prefix: &str) -> error::Result<Vec<String>> {
            let mut keys: Vec<_> = self
                .values
                .lock()
                .unwrap()
                .keys()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect();
            keys.sort();
            Ok(keys)
        }
    }

    fn payment(status: PaymentStatus) -> PaymentRecord {
        PaymentRecord {
            id: "payment-id".to_owned(),
            payment_hash: "payment-hash".to_owned(),
            direction: PaymentDirection::Outbound,
            amount_msat: 1_000,
            fee_msat: Some(10),
            status,
            created_at: 42,
            invoice: None,
        }
    }

    #[test]
    fn storage_keys_round_trip() {
        let encoded = storage_key("primary", "secondary", "key");
        assert_eq!(
            split_storage_key(&encoded).unwrap(),
            (
                "primary".to_owned(),
                "secondary".to_owned(),
                "key".to_owned()
            )
        );
    }

    #[test]
    fn empty_secondary_namespace_round_trips() {
        let encoded = storage_key("payments", "", "0001-id");
        assert_eq!(
            split_storage_key(&encoded).unwrap(),
            ("payments".to_owned(), String::new(), "0001-id".to_owned())
        );
    }

    #[test]
    fn backend_round_trips_keys_and_reports_vss_kind() {
        let store = VssStore::with_client(Box::new(FakeClient::default())).unwrap();
        assert_eq!(store.kind(), PersistenceKind::Vss);

        store
            .write("primary", "secondary", "key", b"value".to_vec())
            .unwrap();
        assert_eq!(store.read("primary", "secondary", "key").unwrap(), b"value");
        assert_eq!(
            store.list("primary", "secondary").unwrap(),
            vec!["key".to_owned()]
        );
        assert_eq!(
            store.list_all_keys().unwrap(),
            vec![(
                "primary".to_owned(),
                "secondary".to_owned(),
                "key".to_owned()
            )]
        );

        store.remove("primary", "secondary", "key", false).unwrap();
        assert_eq!(
            store
                .read("primary", "secondary", "key")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn corrupt_payment_history_is_skipped() {
        let client = FakeClient::default();
        client
            .write(
                &storage_key(PAYMENTS_NAMESPACE, "", "bad"),
                b"not-json".to_vec(),
            )
            .unwrap();
        client
            .write(
                &storage_key(PAYMENTS_NAMESPACE, "", "payment-id"),
                json::to_vec(&payment(PaymentStatus::Succeeded)).unwrap(),
            )
            .unwrap();
        let store = VssStore::with_client(Box::new(client)).unwrap();
        assert_eq!(
            store.get_payment("payment-id").unwrap().unwrap().status,
            PaymentStatus::Succeeded
        );
        assert!(store.get_payment("bad").unwrap().is_none());
    }

    #[test]
    fn remote_vss_without_a_token_is_refused() {
        let err = match VssStore::new(
            "https://vss.example.test",
            "store",
            [7u8; 32],
            None,
            std::path::PathBuf::from("unused-vss-version"),
        ) {
            Ok(_) => panic!("remote VSS without a token must be refused"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("storage-auth-token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn plaintext_vss_is_limited_to_loopback_hosts() {
        for url in [
            "http://localhost:8080",
            "http://127.0.0.2:8080",
            "http://[::1]:8080",
        ] {
            assert_eq!(validate_vss_url(url).unwrap(), true, "{url}");
        }
        for url in [
            "http://0.0.0.0:8080",
            "http://192.168.1.2:8080",
            "http://example.test",
            "http://[::]:8080",
        ] {
            let err = validate_vss_url(url).unwrap_err().to_string();
            assert!(err.contains("https"), "unexpected error for {url}: {err}");
        }
        assert!(!validate_vss_url("https://example.test").unwrap());
    }

    #[test]
    fn canonical_vss_url_omits_credentials_and_normalizes_endpoint() {
        let first =
            canonical_vss_url("https://user:pass@VSS.EXAMPLE.test:443/api/?token=one#secret")
                .unwrap();
        let second = canonical_vss_url("https://vss.example.test/api?token=two").unwrap();

        assert_eq!(first, "https://vss.example.test/api");
        assert_eq!(first, second);
        assert_eq!(
            canonical_vss_url("http://[::1]:8080/").unwrap(),
            "http://[::1]:8080"
        );
    }

    #[test]
    fn payments_are_seeded_and_upserted_by_id() {
        let client = FakeClient::default();
        client
            .write(
                &storage_key(PAYMENTS_NAMESPACE, "", "payment-id"),
                json::to_vec(&payment(PaymentStatus::Pending)).unwrap(),
            )
            .unwrap();
        let store = VssStore::with_client(Box::new(client.clone())).unwrap();
        assert_eq!(
            store.get_payment("payment-id").unwrap().unwrap().status,
            PaymentStatus::Pending
        );

        store
            .upsert_payment(&payment(PaymentStatus::Succeeded))
            .unwrap();
        assert_eq!(
            store.get_payment("payment-id").unwrap().unwrap().status,
            PaymentStatus::Succeeded
        );
        assert_eq!(
            client
                .list(&namespace_prefix(PAYMENTS_NAMESPACE, ""))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn circular_payment_directions_have_distinct_ids() {
        let client = FakeClient::default();
        let store = VssStore::with_client(Box::new(client.clone())).unwrap();
        let payment_hash = "same-payment-hash";
        let mut inbound = payment(PaymentStatus::Succeeded);
        inbound.id = PaymentRecord::direction_qualified_id(PaymentDirection::Inbound, payment_hash);
        inbound.payment_hash = payment_hash.to_owned();
        inbound.direction = PaymentDirection::Inbound;
        let mut outbound = payment(PaymentStatus::Succeeded);
        outbound.id =
            PaymentRecord::direction_qualified_id(PaymentDirection::Outbound, payment_hash);
        outbound.payment_hash = payment_hash.to_owned();

        store.upsert_payment(&inbound).unwrap();
        store.upsert_payment(&outbound).unwrap();

        let records = store.list_payments(&PaymentFilter::default()).unwrap();
        assert_eq!(records.len(), 2);
        assert!(store.get_payment(&inbound.id).unwrap().is_some());
        assert!(store.get_payment(&outbound.id).unwrap().is_some());
        assert!(records
            .iter()
            .all(|record| record.payment_hash == payment_hash));
        assert_eq!(
            client
                .list(&namespace_prefix(PAYMENTS_NAMESPACE, ""))
                .unwrap()
                .len(),
            2
        );
    }
}
