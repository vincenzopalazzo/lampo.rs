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

mod vss;

use vss::VssClient;

/// VSS-backed Lampo persistence.
pub struct VssStore {
    client: Box<dyn StoreClient>,
    payments: Mutex<HashMap<String, PaymentRecord>>,
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
    pub fn new(base_url: &str, store_id: &str) -> error::Result<Self> {
        let client: Box<dyn StoreClient> = Box::new(VssClient::new(base_url, store_id)?);
        let payments = load_payments(client.as_ref())?;
        Ok(Self {
            client,
            payments: Mutex::new(payments),
        })
    }

    #[cfg(test)]
    fn with_client(client: Box<dyn StoreClient>) -> error::Result<Self> {
        let payments = load_payments(client.as_ref())?;
        Ok(Self {
            client,
            payments: Mutex::new(payments),
        })
    }
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
        let encoded = client
            .read(&key)
            .map_err(|err| error::anyhow!("reading VSS payment `{key}`: {err}"))?;
        let payment: PaymentRecord = json::from_slice(&encoded)?;
        payments.insert(payment.id.clone(), payment);
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
        let mut payments = self
            .payments
            .lock()
            .map_err(|err| error::anyhow!("payment map poisoned: {err}"))?;

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
            .payments
            .lock()
            .map_err(|err| error::anyhow!("payment map poisoned: {err}"))?
            .get(id)
            .cloned())
    }

    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
        let mut matched: Vec<_> = self
            .payments
            .lock()
            .map_err(|err| error::anyhow!("payment map poisoned: {err}"))?
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
}
