//! TLS certificate lifecycle for the LND-compatible REST listener.
//!
//! Certificates are generated once and reused across restarts so Zeus (and any
//! cert pinning) keeps trusting the same identity.

use std::fs::{self, OpenOptions};
use std::io::{BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls error: {0}")]
    Other(String),
}

#[derive(Clone, Debug)]
pub struct TlsMaterial {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

impl TlsMaterial {
    pub fn load_or_create(dir: impl AsRef<Path>, hostnames: &[String]) -> Result<Self, TlsError> {
        let dir = dir.as_ref();
        ensure_secure_dir(dir)?;
        let cert_path = dir.join("tls.cert");
        let key_path = dir.join("tls.key");
        let sans_path = dir.join("tls.sans");
        let expected_sans = normalized_sans(hostnames);

        if cert_path.exists() || key_path.exists() {
            if !(cert_path.exists() && key_path.exists()) {
                return Err(TlsError::Other(
                    "incomplete TLS material: both tls.cert and tls.key are required".into(),
                ));
            }
            reject_symlink(&cert_path)?;
            reject_symlink(&key_path)?;
            if !sans_path.exists() {
                return Err(TlsError::Other(format!(
                    "existing TLS material has no SAN manifest; remove {}, {}, and {} \
                     to regenerate it with the configured lnd-tls-san values",
                    cert_path.display(),
                    key_path.display(),
                    sans_path.display()
                )));
            }
            reject_symlink(&sans_path)?;
            let actual_sans = fs::read_to_string(&sans_path)?;
            if actual_sans != expected_sans {
                return Err(TlsError::Other(format!(
                    "configured TLS SANs changed; remove {}, {}, and {} to generate a new \
                     certificate identity, then trust the new certificate in remote clients",
                    cert_path.display(),
                    key_path.display(),
                    sans_path.display()
                )));
            }
            return Ok(Self {
                cert_path,
                key_path,
            });
        }

        let mut params =
            CertificateParams::new(Vec::new()).map_err(|e| TlsError::Other(e.to_string()))?;
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "lampo-lnd");

        let mut sans = Vec::new();
        for host in hostnames {
            if let Ok(ip) = host.parse() {
                sans.push(SanType::IpAddress(ip));
            } else {
                sans.push(SanType::DnsName(
                    host.clone()
                        .try_into()
                        .map_err(|e| TlsError::Other(format!("{e}")))?,
                ));
            }
        }
        if sans.is_empty() {
            sans.push(SanType::DnsName(
                "localhost"
                    .to_string()
                    .try_into()
                    .map_err(|e| TlsError::Other(format!("{e}")))?,
            ));
            sans.push(SanType::IpAddress(std::net::IpAddr::V4(
                std::net::Ipv4Addr::LOCALHOST,
            )));
        }
        params.subject_alt_names = sans;

        let key_pair = KeyPair::generate().map_err(|e| TlsError::Other(e.to_string()))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| TlsError::Other(e.to_string()))?;

        write_secret_file(&cert_path, cert.pem().as_bytes())?;
        write_secret_file(&key_path, key_pair.serialize_pem().as_bytes())?;
        write_secret_file(&sans_path, expected_sans.as_bytes())?;

        Ok(Self {
            cert_path,
            key_path,
        })
    }

    pub fn server_config(&self) -> Result<ServerConfig, TlsError> {
        let cert_file = fs::File::open(&self.cert_path)?;
        let mut cert_reader = BufReader::new(cert_file);
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| TlsError::Other(e.to_string()))?;

        let key_file = fs::File::open(&self.key_path)?;
        let mut key_reader = BufReader::new(key_file);
        let key = rustls_pemfile::private_key(&mut key_reader)
            .map_err(|e| TlsError::Other(e.to_string()))?
            .ok_or_else(|| TlsError::Other("tls.key contained no private key".into()))?;

        let key = match key {
            PrivateKeyDer::Pkcs8(k) => {
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(k.secret_pkcs8_der().to_vec()))
            }
            other => other,
        };

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| TlsError::Other(e.to_string()))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }

    pub fn server_config_arc(&self) -> Result<Arc<ServerConfig>, TlsError> {
        Ok(Arc::new(self.server_config()?))
    }
}

fn normalized_sans(hostnames: &[String]) -> String {
    let mut hostnames = hostnames
        .iter()
        .filter(|hostname| !hostname.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    hostnames.sort();
    hostnames.dedup();
    hostnames.join("\n")
}

fn ensure_secure_dir(path: &Path) -> Result<(), TlsError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), TlsError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(TlsError::Other(format!(
            "refusing symlink TLS path {}",
            path.display()
        )));
    }
    Ok(())
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), TlsError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn stable_across_restart() {
        let dir = tempdir().unwrap();
        let first = TlsMaterial::load_or_create(dir.path(), &["127.0.0.1".into()]).unwrap();
        let cert1 = fs::read(&first.cert_path).unwrap();
        let second = TlsMaterial::load_or_create(dir.path(), &["127.0.0.1".into()]).unwrap();
        let cert2 = fs::read(&second.cert_path).unwrap();
        assert_eq!(cert1, cert2);
    }

    #[test]
    fn rejects_stale_certificate_sans() {
        let dir = tempdir().unwrap();
        TlsMaterial::load_or_create(dir.path(), &["127.0.0.1".into()]).unwrap();

        let err =
            TlsMaterial::load_or_create(dir.path(), &["127.0.0.1".into(), "node.local".into()])
                .unwrap_err();
        assert!(err.to_string().contains("configured TLS SANs changed"));
    }
}
