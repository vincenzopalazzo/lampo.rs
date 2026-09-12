//! BLIP-42 contact book for Lampo.
//!
//! Persistence is intentionally simple (JSON under the network data dir) so we
//! can iterate the UX while the LDK fork API is still settling. Secrets are
//! stored as hex; encrypt-at-rest is tracked as a follow-up.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lampo_common::error;
use lampo_common::hex;
use lampo_common::json;
use lampo_common::ldk::offers::contacts::{ContactSecret, ContactSecrets};
use lampo_common::ldk::offers::offer::Offer;
use serde::{Deserialize, Serialize};

const CONTACTS_FILE: &str = "contacts.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub label: String,
    /// Hex-encoded primary contact secret we send when paying this contact.
    pub primary_secret_hex: String,
    /// Offer we use (or received) as the return path to this contact.
    pub remote_offer: String,
    /// Compact offer we revealed to this contact (if any), for payback matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub our_offer: Option<String>,
    /// Nonce hex used to build `our_offer` (needed to re-derive signing keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub our_offer_nonce_hex: Option<String>,
    /// Extra remote secrets attributed to this contact after reconciliation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_remote_secrets_hex: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ContactBook {
    contacts: HashMap<String, Contact>,
}

pub struct ContactStore {
    path: PathBuf,
    inner: Mutex<ContactBook>,
}

impl ContactStore {
    pub fn open(data_dir: &Path) -> error::Result<Self> {
        let path = data_dir.join(CONTACTS_FILE);
        let book = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            json::from_str(&raw).unwrap_or_default()
        } else {
            ContactBook::default()
        };
        Ok(Self {
            path,
            inner: Mutex::new(book),
        })
    }

    pub fn list(&self) -> Vec<Contact> {
        let book = self.inner.lock().unwrap();
        book.contacts.values().cloned().collect()
    }

    pub fn get(&self, label: &str) -> Option<Contact> {
        self.inner.lock().unwrap().contacts.get(label).cloned()
    }

    pub fn upsert(&self, contact: Contact) -> error::Result<()> {
        let mut book = self.inner.lock().unwrap();
        book.contacts.insert(contact.label.clone(), contact);
        self.persist(&book)
    }

    /// Find a contact whose primary or additional secret matches `secret`.
    pub fn find_by_secret(&self, secret: &ContactSecret) -> Option<Contact> {
        let needle = hex::encode(secret.as_bytes());
        let book = self.inner.lock().unwrap();
        book.contacts
            .values()
            .find(|c| {
                c.primary_secret_hex == needle
                    || c.additional_remote_secrets_hex.iter().any(|s| s == &needle)
            })
            .cloned()
    }

    pub fn secrets_for(&self, contact: &Contact) -> error::Result<ContactSecrets> {
        let primary = decode_secret(&contact.primary_secret_hex)?;
        let mut secrets = ContactSecrets::new(primary);
        for extra in &contact.additional_remote_secrets_hex {
            secrets.add_remote_secret(decode_secret(extra)?);
        }
        Ok(secrets)
    }

    pub fn remember_inbound(
        &self,
        label: &str,
        secret: ContactSecret,
        payer_offer: Option<&Offer>,
    ) -> error::Result<Contact> {
        if let Some(existing) = self.find_by_secret(&secret) {
            if let Some(offer) = payer_offer {
                // BLIP-42: ignore offer updates when the secret already matches.
                let _ = offer;
            }
            return Ok(existing);
        }
        if let Some(mut existing) = self.get(label) {
            let hex = hex::encode(secret.as_bytes());
            if existing.primary_secret_hex != hex
                && !existing.additional_remote_secrets_hex.contains(&hex)
            {
                existing.additional_remote_secrets_hex.push(hex);
                self.upsert(existing.clone())?;
            }
            return Ok(existing);
        }
        let contact = Contact {
            label: label.to_string(),
            primary_secret_hex: hex::encode(secret.as_bytes()),
            remote_offer: payer_offer
                .map(|o| o.to_string())
                .ok_or_else(|| error::anyhow!("inbound contact requires a payer_offer return path"))?,
            our_offer: None,
            our_offer_nonce_hex: None,
            additional_remote_secrets_hex: Vec::new(),
        };
        self.upsert(contact.clone())?;
        Ok(contact)
    }

    fn persist(&self, book: &ContactBook) -> error::Result<()> {
        let raw = json::to_string_pretty(book)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, raw)?;
        Ok(())
    }
}

fn decode_secret(hex_str: &str) -> error::Result<ContactSecret> {
    let bytes = hex::decode(hex_str)?;
    if bytes.len() != 32 {
        error::bail!("contact secret must be 32 bytes, got {}", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(ContactSecret::new(arr))
}
