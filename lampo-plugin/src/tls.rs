//! TLS certificate generation and management for remote plugins.
//!
//! On first run, lampo generates a self-signed CA and uses it to
//! issue client certificates. Remote plugins must present a server
//! certificate signed by the same CA (or a user-provided CA).
//!
//! Tonic handles the actual TLS handshake; this module only manages
//! PEM files on disk.
#![cfg(feature = "grpc")]

use std::fs;
use std::path::PathBuf;

use lampo_common::error;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};

/// Directory layout for plugin certificates.
///
/// ```text
/// <lampo_dir>/plugin-certs/
///   ca.pem           — CA certificate
///   ca-key.pem       — CA private key
///   client.pem       — Client certificate (daemon identity)
///   client-key.pem   — Client private key
/// ```
pub struct CertStore {
    base_dir: PathBuf,
}

impl CertStore {
    pub fn new(lampo_dir: &str) -> Self {
        Self {
            base_dir: PathBuf::from(lampo_dir).join("plugin-certs"),
        }
    }

    /// Ensure the cert directory and CA exist. Generate if missing.
    pub fn ensure_initialized(&self) -> error::Result<()> {
        fs::create_dir_all(&self.base_dir)?;

        if !self.ca_cert_path().exists() {
            log::info!(target: "plugin::tls", "generating new CA for plugin mTLS");
            self.generate_ca()?;
        }

        if !self.client_cert_path().exists() {
            log::info!(target: "plugin::tls", "generating client certificate for daemon");
            self.generate_client_cert()?;
        }

        Ok(())
    }

    pub fn ca_cert_path(&self) -> PathBuf {
        self.base_dir.join("ca.pem")
    }

    pub fn ca_key_path(&self) -> PathBuf {
        self.base_dir.join("ca-key.pem")
    }

    pub fn client_cert_path(&self) -> PathBuf {
        self.base_dir.join("client.pem")
    }

    pub fn client_key_path(&self) -> PathBuf {
        self.base_dir.join("client-key.pem")
    }

    /// Read CA certificate PEM.
    pub fn ca_cert_pem(&self) -> error::Result<String> {
        Ok(fs::read_to_string(self.ca_cert_path())?)
    }

    /// Read client certificate PEM.
    pub fn client_cert_pem(&self) -> error::Result<String> {
        Ok(fs::read_to_string(self.client_cert_path())?)
    }

    /// Read client key PEM.
    pub fn client_key_pem(&self) -> error::Result<String> {
        Ok(fs::read_to_string(self.client_key_path())?)
    }

    fn generate_ca(&self) -> error::Result<()> {
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "lampo-plugin-ca");
        dn.push(DnType::OrganizationName, "lampo");
        params.distinguished_name = dn;
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);

        let key_pair = KeyPair::generate()?;
        let cert = params.self_signed(&key_pair)?;

        fs::write(self.ca_cert_path(), cert.pem())?;
        fs::write(self.ca_key_path(), key_pair.serialize_pem())?;

        // Restrict permissions on the CA key
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(self.ca_key_path(), fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    fn generate_client_cert(&self) -> error::Result<()> {
        // Load CA
        let ca_cert_pem = fs::read_to_string(self.ca_cert_path())?;
        let ca_key_pem = fs::read_to_string(self.ca_key_path())?;

        let ca_key = KeyPair::from_pem(&ca_key_pem)?;
        let ca_params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)?;
        let ca_cert = ca_params.self_signed(&ca_key)?;

        // Generate client cert
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "lampo-daemon");
        dn.push(DnType::OrganizationName, "lampo");
        params.distinguished_name = dn;

        let client_key = KeyPair::generate()?;
        let client_cert = params.signed_by(&client_key, &ca_cert, &ca_key)?;

        fs::write(self.client_cert_path(), client_cert.pem())?;
        fs::write(self.client_key_path(), client_key.serialize_pem())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                self.client_key_path(),
                fs::Permissions::from_mode(0o600),
            )?;
        }

        Ok(())
    }
}
