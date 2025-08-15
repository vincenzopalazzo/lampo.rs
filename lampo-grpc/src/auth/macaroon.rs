use std::fs;
use std::path::{Path, PathBuf};
use tonic::{Request, Status};
use macaroon::{Macaroon, MacaroonKey};
use rand::RngCore;
use base64::{Engine as _, engine::general_purpose::URL_SAFE as BASE64};

#[derive(Debug, PartialEq)]
pub enum MacaroonPermission {
    Admin,
    Readonly,
}

pub struct MacaroonManager {
    admin_macaroon_path: PathBuf,
    readonly_macaroon_path: PathBuf,
    root_key: [u8; 32],
}

impl MacaroonManager {
    pub fn new(data_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let admin_macaroon_path = data_dir.join("admin.macaroon");
        let readonly_macaroon_path = data_dir.join("readonly.macaroon");
        let root_key_path = data_dir.join("macaroon.key");
        
        // Generate or load root key
        let root_key = if root_key_path.exists() {
            let key_bytes = fs::read(&root_key_path)?;
            if key_bytes.len() != 32 {
                return Err("Invalid root key length".into());
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            key
        } else {
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            fs::write(&root_key_path, &key)?;
            key
        };

        // Create macaroons if they don't exist
        if !admin_macaroon_path.exists() {
            let admin_macaroon = Self::create_admin_macaroon(&root_key)?;
            let serialized = admin_macaroon.serialize(macaroon::Format::V1)?;
            let binary_data = BASE64.decode(&serialized)?;
            fs::write(&admin_macaroon_path, binary_data)?;
        }

        if !readonly_macaroon_path.exists() {
            let readonly_macaroon = Self::create_readonly_macaroon(&root_key)?;
            let serialized = readonly_macaroon.serialize(macaroon::Format::V1)?;
            let binary_data = BASE64.decode(&serialized)?;
            fs::write(&readonly_macaroon_path, binary_data)?;
        }

        log::info!("Macaroon authentication initialized");
        log::info!("Admin macaroon: {:?}", admin_macaroon_path);
        log::info!("Readonly macaroon: {:?}", readonly_macaroon_path);

        Ok(Self {
            admin_macaroon_path,
            readonly_macaroon_path,
            root_key,
        })
    }

    fn create_admin_macaroon(root_key: &[u8; 32]) -> Result<Macaroon, Box<dyn std::error::Error>> {
        let key = MacaroonKey::generate(root_key);
        let mut macaroon = Macaroon::create(
            Some("lampo".to_string().into()),
            &key,
            "admin".to_string().into(),
        )?;
        macaroon.add_first_party_caveat("admin".to_string().into());
        Ok(macaroon)
    }

    fn create_readonly_macaroon(root_key: &[u8; 32]) -> Result<Macaroon, Box<dyn std::error::Error>> {
        let key = MacaroonKey::generate(root_key);
        let mut macaroon = Macaroon::create(
            Some("lampo".to_string().into()),
            &key,
            "readonly".to_string().into(),
        )?;
        macaroon.add_first_party_caveat("readonly".to_string().into());
        Ok(macaroon)
    }

    pub fn validate_request<T>(&self, request: &Request<T>) -> Result<MacaroonPermission, Status> {
        let metadata = request.metadata();
        
        // Look for macaroon in metadata
        if let Some(macaroon_value) = metadata.get("macaroon") {
            let macaroon_hex = macaroon_value.to_str()
                .map_err(|_| Status::unauthenticated("Invalid macaroon encoding"))?;
            
            let macaroon_bytes = hex::decode(macaroon_hex)
                .map_err(|_| Status::unauthenticated("Invalid macaroon hex"))?;
            
            let macaroon = Macaroon::deserialize_binary(&macaroon_bytes)
                .map_err(|_| Status::unauthenticated("Invalid macaroon format"))?;
            
            // TODO: for now this is simplified version
            let identifier_bytes = macaroon.identifier();
            let identifier = String::from_utf8_lossy(identifier_bytes.as_ref());
            
            match identifier.as_ref() {
                "admin" => Ok(MacaroonPermission::Admin),
                "readonly" => Ok(MacaroonPermission::Readonly),
                _ => Err(Status::unauthenticated("Unknown macaroon type")),
            }
        } else {
            Err(Status::unauthenticated("No macaroon provided"))
        }
    }
}