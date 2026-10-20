//! BLIP-42 contact book for Lampo.
//!
//! The book is `HashMap<ContactId, ContactInfo>`. `ContactInfo` is Lampo's
//! record: how we reach them (their offer or their BIP 353 name) plus any
//! secret we cannot recompute. The signing key is `ContactId::signing_secret`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use lampo_common::bitcoin::secp256k1::Secp256k1;
use lampo_common::error;
use lampo_common::hex;
use lampo_common::json;
use lampo_common::ldk::offers::contacts::{
    compute_contact_secret, ContactId, ContactSecret, ContactSecrets,
};
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::onion_message::dns_resolution::HumanReadableName;
use serde::{Deserialize, Serialize};

const CONTACTS_FILE: &str = "contacts.json";

/// How we reach one contact. Their offer, or their BIP 353 name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContactDest {
    Offer {
        offer: String,
    },
    Bip353 {
        name: String,
        /// Last resolved offer, so the secret can be recomputed without DNS.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offer: Option<String>,
    },
}

/// One contact, keyed by the persisted [`ContactId`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactInfo {
    pub label: String,
    /// `entropy:<hex>` or `bip353:<user>@<domain>`.
    pub contact_id: String,
    pub dest: ContactDest,
    /// Secret they sent, when they paid us first. Pay sends this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_secret_hex: Option<String>,
    /// Secrets from an older offer node id, so those payments still match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_remote_secrets_hex: Vec<String>,
}

impl ContactInfo {
    pub fn id(&self) -> error::Result<ContactId> {
        decode_contact_id(&self.contact_id)
    }

    pub fn remote_offer_str(&self) -> Option<&str> {
        match &self.dest {
            ContactDest::Offer { offer } => Some(offer.as_str()),
            ContactDest::Bip353 { offer, .. } => offer.as_deref(),
        }
    }

    pub fn bip353(&self) -> Option<&str> {
        match &self.dest {
            ContactDest::Bip353 { name, .. } => Some(name.as_str()),
            ContactDest::Offer { .. } => None,
        }
    }

    pub fn signing_pubkey(&self) -> error::Result<String> {
        let secp = Secp256k1::signing_only();
        Ok(self.id()?.signing_pubkey(&secp).to_string())
    }

    pub fn their_offer(&self) -> error::Result<Option<Offer>> {
        match self.remote_offer_str() {
            Some(offer) => Ok(Some(parse_offer(offer)?)),
            None => Ok(None),
        }
    }

    /// Secret we send when paying. Recomputed from [`ContactId`] unless they paid first.
    pub fn secrets(&self) -> error::Result<ContactSecrets> {
        let mut secrets = if let Some(hex_str) = &self.remote_secret_hex {
            ContactSecrets::from_remote_secret(decode_secret(hex_str)?)
        } else {
            let offer = self.their_offer()?.ok_or_else(|| {
                error::anyhow!("contact `{}` has no offer to derive a secret from", self.label)
            })?;
            compute_contact_secret(&self.id()?.signing_secret(), &offer)
                .map_err(|err| error::anyhow!("compute_contact_secret: {:?}", err))?
        };
        for extra in &self.additional_remote_secrets_hex {
            secrets.add_remote_secret(decode_secret(extra)?);
        }
        Ok(secrets)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ContactBook {
    /// Keyed by the encoded [`ContactId`].
    contacts: HashMap<String, ContactInfo>,
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

    pub fn list(&self) -> Vec<ContactInfo> {
        let book = self.inner.lock().unwrap();
        book.contacts.values().cloned().collect()
    }

    pub fn get(&self, label: &str) -> Option<ContactInfo> {
        self.inner
            .lock()
            .unwrap()
            .contacts
            .values()
            .find(|contact| contact.label == label)
            .cloned()
    }

    pub fn find_by_secret(&self, secret: &ContactSecret) -> Option<ContactInfo> {
        let book = self.inner.lock().unwrap();
        book.contacts.values().find(|contact| match contact.secrets() {
            Ok(secrets) => secrets.matches(secret),
            Err(_) => false,
        }).cloned()
    }

    /// They paid us first. The key is a fresh [`ContactId`]; the value is the offer they revealed.
    pub fn remember_inbound(
        &self,
        label: &str,
        id: ContactId,
        secret: ContactSecret,
        payer_offer: Option<&Offer>,
    ) -> error::Result<ContactInfo> {
        if let Some(existing) = self.find_by_secret(&secret) {
            return Ok(existing);
        }
        let secret_hex = hex::encode(secret.as_bytes());
        if let Some(mut existing) = self.get(label) {
            if existing.remote_secret_hex.as_deref() != Some(secret_hex.as_str())
                && !existing.additional_remote_secrets_hex.contains(&secret_hex)
            {
                existing.additional_remote_secrets_hex.push(secret_hex);
                self.upsert(existing.clone())?;
            }
            return Ok(existing);
        }
        let offer = payer_offer
            .map(|offer| offer.to_string())
            .ok_or_else(|| error::anyhow!("inbound contact requires a payer_offer return path"))?;
        let contact = ContactInfo {
            label: label.to_string(),
            contact_id: encode_contact_id(&id),
            dest: ContactDest::Offer { offer },
            remote_secret_hex: Some(secret_hex),
            additional_remote_secrets_hex: Vec::new(),
        };
        self.upsert(contact.clone())?;
        Ok(contact)
    }

    /// We added them. Persist `id` and their offer. The signing key is derived from `id`.
    pub fn remember_outbound(
        &self,
        label: &str,
        id: &ContactId,
        their_offer: &Offer,
    ) -> error::Result<ContactInfo> {
        if self.get(label).is_some() {
            return self.note_their_offer(label, their_offer);
        }
        let contact = ContactInfo {
            label: label.to_string(),
            contact_id: encode_contact_id(id),
            dest: ContactDest::Offer {
                offer: their_offer.to_string(),
            },
            remote_secret_hex: None,
            additional_remote_secrets_hex: Vec::new(),
        };
        self.upsert(contact.clone())?;
        Ok(contact)
    }

    /// Store a BIP 353 name under a fresh entropy [`ContactId`].
    pub fn remember_bip353(
        &self,
        label: &str,
        id: &ContactId,
        name: &str,
    ) -> error::Result<ContactInfo> {
        if let Some(existing) = self.get(label) {
            return Ok(existing);
        }
        let contact = ContactInfo {
            label: label.to_string(),
            contact_id: encode_contact_id(id),
            dest: ContactDest::Bip353 {
                name: name.to_string(),
                offer: None,
            },
            remote_secret_hex: None,
            additional_remote_secrets_hex: Vec::new(),
        };
        self.upsert(contact.clone())?;
        Ok(contact)
    }

    /// Replace their offer. Keep the previous secret when the offer node id changes.
    pub fn note_their_offer(&self, label: &str, their_offer: &Offer) -> error::Result<ContactInfo> {
        let mut contact = self
            .get(label)
            .ok_or_else(|| error::anyhow!("unknown contact `{label}`"))?;
        let new_offer = their_offer.to_string();
        if contact.remote_offer_str() == Some(new_offer.as_str()) {
            return Ok(contact);
        }
        if contact.remote_secret_hex.is_none() {
            if let Some(old) = contact.their_offer()? {
                if let Ok(old_secrets) =
                    compute_contact_secret(&contact.id()?.signing_secret(), &old)
                {
                    let old_hex = hex::encode(old_secrets.primary_secret().as_bytes());
                    if let Ok(new_secrets) =
                        compute_contact_secret(&contact.id()?.signing_secret(), their_offer)
                    {
                        let new_hex = hex::encode(new_secrets.primary_secret().as_bytes());
                        if old_hex != new_hex
                            && !contact.additional_remote_secrets_hex.contains(&old_hex)
                        {
                            contact.additional_remote_secrets_hex.push(old_hex);
                        }
                    }
                }
            }
        }
        match &mut contact.dest {
            ContactDest::Offer { offer } => *offer = new_offer,
            ContactDest::Bip353 { offer, .. } => *offer = Some(new_offer),
        }
        self.upsert(contact.clone())?;
        Ok(contact)
    }

    fn upsert(&self, contact: ContactInfo) -> error::Result<()> {
        let mut book = self.inner.lock().unwrap();
        book.contacts.insert(contact.contact_id.clone(), contact);
        self.persist(&book)
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

pub fn encode_contact_id(id: &ContactId) -> String {
    match id {
        ContactId::Entropy(bytes) => format!("entropy:{}", hex::encode(bytes)),
        ContactId::Bip353(name) => format!("bip353:{}@{}", name.user(), name.domain()),
    }
}

pub fn decode_contact_id(raw: &str) -> error::Result<ContactId> {
    if let Some(hex_str) = raw.strip_prefix("entropy:") {
        let bytes = hex::decode(hex_str)?;
        if bytes.len() != 32 {
            error::bail!("contact id entropy must be 32 bytes");
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        return Ok(ContactId::Entropy(arr));
    }
    if let Some(name) = raw.strip_prefix("bip353:") {
        let hrn = HumanReadableName::from_encoded(name)
            .map_err(|()| error::anyhow!("invalid bip353 name `{name}`"))?;
        return Ok(ContactId::from_bip353(hrn));
    }
    error::bail!("contact id `{raw}` is not entropy or bip353")
}

fn parse_offer(raw: &str) -> error::Result<Offer> {
    Offer::from_str(raw).map_err(|err| error::anyhow!("invalid offer: {:?}", err))
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
