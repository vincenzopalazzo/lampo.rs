//! Server-side store for static invoices.
//!
//! A lampo node configured with `async-payments-role=server` persists the
//! [`StaticInvoice`]s that often-offline recipients ask it to serve, and hands
//! them to payers when an invoice request arrives. The design (storage layout,
//! rate limiting) is ported from ldk-node's
//! `src/payment/asynchronous/static_invoice_store.rs`; the encoding is manual
//! because lampod does not depend on the `lightning` crate directly, so LDK's
//! TLV macros do not resolve here.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lampo_common::bitcoin::hashes::sha256::Hash as Sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::error;
use lampo_common::ldk::blinded_path::message::BlindedMessagePath;
use lampo_common::ldk::io::{Cursor, ErrorKind};
use lampo_common::ldk::offers::static_invoice::StaticInvoice;
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::ldk::util::ser::{LengthReadable, Readable, Writeable};

use crate::persistence::LampoPersistence;

/// Invoices live at `static_invoices/<hex sha256(recipient_id)>/<slot>`, e.g.
/// `static_invoices/039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81/00001`.
const STATIC_INVOICE_STORE_PRIMARY_NAMESPACE: &str = "static_invoices";

/// Layout version of a stored record, so the format can change later without
/// misreading old entries.
const RECORD_VERSION: u8 = 1;

struct PersistedStaticInvoice {
    invoice: StaticInvoice,
    request_path: BlindedMessagePath,
}

impl PersistedStaticInvoice {
    /// version, then a u32 length-prefixed invoice (`StaticInvoice` is only
    /// `LengthReadable`, so it cannot delimit itself on decode), then the
    /// request path, which is self-delimiting.
    fn encode(&self) -> Vec<u8> {
        let mut buf = vec![RECORD_VERSION];
        let invoice = self.invoice.encode();
        buf.extend_from_slice(&(invoice.len() as u32).to_be_bytes());
        buf.extend_from_slice(&invoice);
        buf.extend_from_slice(&self.request_path.encode());
        buf
    }

    fn decode(buf: &[u8]) -> error::Result<Self> {
        if buf.len() < 5 {
            error::bail!("static invoice record too short: {} bytes", buf.len());
        }
        if buf[0] != RECORD_VERSION {
            error::bail!("unsupported static invoice record version {}", buf[0]);
        }
        let invoice_len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if buf.len() < 5 + invoice_len {
            error::bail!(
                "static invoice record truncated: want {invoice_len} invoice bytes, have {}",
                buf.len() - 5
            );
        }
        let invoice = StaticInvoice::read_from_fixed_length_buffer(&mut &buf[5..5 + invoice_len])
            .map_err(|err| error::anyhow!("decoding static invoice: {err:?}"))?;
        let request_path = BlindedMessagePath::read(&mut Cursor::new(&buf[5 + invoice_len..]))
            .map_err(|err| error::anyhow!("decoding invoice request path: {err:?}"))?;
        Ok(Self {
            invoice,
            request_path,
        })
    }
}

fn storage_location(invoice_slot: u16, recipient_id: &[u8]) -> (String, String) {
    let hash = Sha256::hash(recipient_id).to_byte_array();
    (
        lampo_common::hex::encode(hash),
        format!("{invoice_slot:05}"),
    )
}

/// Rate-limited KV store for the static invoices this node serves.
pub struct StaticInvoiceStore {
    persister: Arc<LampoPersistence>,
    request_rate_limiter: Mutex<RateLimiter>,
    persist_rate_limiter: Mutex<RateLimiter>,
}

impl StaticInvoiceStore {
    const RATE_LIMITER_BUCKET_CAPACITY: u32 = 5;
    const RATE_LIMITER_REFILL_INTERVAL: Duration = Duration::from_millis(100);
    const RATE_LIMITER_MAX_IDLE: Duration = Duration::from_secs(600);

    pub fn new(persister: Arc<LampoPersistence>) -> Self {
        Self {
            persister,
            request_rate_limiter: Mutex::new(RateLimiter::new(
                Self::RATE_LIMITER_BUCKET_CAPACITY,
                Self::RATE_LIMITER_REFILL_INTERVAL,
                Self::RATE_LIMITER_MAX_IDLE,
            )),
            persist_rate_limiter: Mutex::new(RateLimiter::new(
                Self::RATE_LIMITER_BUCKET_CAPACITY,
                Self::RATE_LIMITER_REFILL_INTERVAL,
                Self::RATE_LIMITER_MAX_IDLE,
            )),
        }
    }

    fn allow_rate(limiter: &Mutex<RateLimiter>, recipient_id: &[u8]) -> error::Result<bool> {
        let mut limiter = limiter
            .lock()
            .map_err(|_| error::anyhow!("rate limiter lock poisoned"))?;
        Ok(limiter.allow(recipient_id))
    }

    /// Persist an invoice from an `Event::PersistStaticInvoice`.
    ///
    /// `Ok(true)` means the write landed and the handler may confirm to the
    /// recipient. `Ok(false)` means this recipient is rate-limited: do not
    /// confirm and do not treat it as a store failure (a rate-limit `Err`
    /// would be replayed forever at the head of the LDK event queue).
    pub fn persist(
        &self,
        invoice: StaticInvoice,
        request_path: BlindedMessagePath,
        invoice_slot: u16,
        recipient_id: &[u8],
    ) -> error::Result<bool> {
        if !Self::allow_rate(&self.persist_rate_limiter, recipient_id)? {
            log::debug!(
                target: "lampo::static_invoice_store",
                "rate-limited persist for slot {invoice_slot}; skipping write"
            );
            return Ok(false);
        }
        let (secondary, key) = storage_location(invoice_slot, recipient_id);
        let record = PersistedStaticInvoice {
            invoice,
            request_path,
        };
        self.persister.write(
            STATIC_INVOICE_STORE_PRIMARY_NAMESPACE,
            &secondary,
            &key,
            record.encode(),
        )?;
        Ok(true)
    }

    /// Load an invoice for an `Event::StaticInvoiceRequested`. `Ok(None)`
    /// means we never persisted (or replaced away) that slot.
    pub fn load(
        &self,
        recipient_id: &[u8],
        invoice_slot: u16,
    ) -> error::Result<Option<(StaticInvoice, BlindedMessagePath)>> {
        if !Self::allow_rate(&self.request_rate_limiter, recipient_id)? {
            log::debug!(
                target: "lampo::static_invoice_store",
                "rate-limited load for slot {invoice_slot}; treating as empty"
            );
            return Ok(None);
        }
        let (secondary, key) = storage_location(invoice_slot, recipient_id);
        match self
            .persister
            .read(STATIC_INVOICE_STORE_PRIMARY_NAMESPACE, &secondary, &key)
        {
            Ok(buf) => {
                // A corrupt record cannot be served and cannot be fixed by
                // replaying the event, so it is removed rather than
                // propagated: the slot becomes truly empty, the payer times
                // out, and the recipient's next invoice refresh re-serves it.
                match PersistedStaticInvoice::decode(&buf) {
                    Ok(record) => Ok(Some((record.invoice, record.request_path))),
                    Err(err) => {
                        log::error!(target: "lampo::static_invoice_store", "dropping corrupt static invoice for slot {invoice_slot}: {err}");
                        let (secondary, key) = storage_location(invoice_slot, recipient_id);
                        if let Err(err) = self.persister.remove(
                            STATIC_INVOICE_STORE_PRIMARY_NAMESPACE,
                            &secondary,
                            &key,
                            false,
                        ) {
                            log::error!(target: "lampo::static_invoice_store", "failed to remove corrupt static invoice for slot {invoice_slot}: {err}");
                        }
                        Ok(None)
                    }
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

/// Leaky-bucket rate limiter keyed by user id, ported from ldk-node. One
/// token is added per refill interval up to the capacity; a bucket idle at
/// capacity for longer than the max idle duration is dropped so the map
/// cannot grow without bound.
struct RateLimiter {
    users: HashMap<Vec<u8>, Bucket>,
    capacity: u32,
    refill_interval: Duration,
    max_idle: Duration,
}

const MAX_USERS: usize = 10_000;

struct Bucket {
    tokens: u32,
    last_refill: Instant,
}

impl RateLimiter {
    fn new(capacity: u32, refill_interval: Duration, max_idle: Duration) -> Self {
        Self {
            users: HashMap::new(),
            capacity,
            refill_interval,
            max_idle,
        }
    }

    fn allow(&mut self, user_id: &[u8]) -> bool {
        let now = Instant::now();
        if !self.users.contains_key(user_id) {
            self.garbage_collect(self.max_idle);
            if self.users.len() >= MAX_USERS {
                return false;
            }
        }
        let bucket = self.users.entry(user_id.to_vec()).or_insert(Bucket {
            tokens: self.capacity,
            last_refill: now,
        });
        let elapsed = now.duration_since(bucket.last_refill);
        let tokens_to_add = (elapsed.as_secs_f64() / self.refill_interval.as_secs_f64()) as u32;
        if tokens_to_add > 0 {
            bucket.tokens = (bucket.tokens + tokens_to_add).min(self.capacity);
            bucket.last_refill = now;
        }
        if bucket.tokens > 0 {
            bucket.tokens -= 1;
            true
        } else {
            false
        }
    }

    fn garbage_collect(&mut self, max_idle: Duration) {
        let now = Instant::now();
        self.users
            .retain(|_, bucket| now.duration_since(bucket.last_refill) < max_idle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_drains_and_refills() {
        let mut limiter = RateLimiter::new(3, Duration::from_millis(100), Duration::from_secs(1));
        assert!(limiter.allow(b"user1"));
        assert!(limiter.allow(b"user1"));
        assert!(limiter.allow(b"user1"));
        assert!(!limiter.allow(b"user1"));
        assert!(limiter.allow(b"user2"));

        std::thread::sleep(Duration::from_millis(150));
        assert!(limiter.allow(b"user1"));
    }

    #[test]
    fn rate_limiter_rejects_when_full_of_users() {
        let mut limiter = RateLimiter::new(1, Duration::from_secs(60), Duration::from_secs(3600));
        for i in 0..MAX_USERS {
            let user = format!("user{i}");
            assert!(limiter.allow(user.as_bytes()));
        }
        assert!(!limiter.allow(b"one-user-too-many"));
    }
}
