use std::fs;
use std::path::{Path, PathBuf};
use rcgen::generate_simple_self_signed;

pub struct TlsManager {
    cert_path: PathBuf,
    key_path: PathBuf,
}

impl TlsManager {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            cert_path: data_dir.join("tls.cert"),
            key_path: data_dir.join("tls.key"),
        }
    }
    
    pub fn ensure_certificates(&self) -> Result<(String, String), Box<dyn std::error::Error>> {
        // Check if certificates already exist
        if self.cert_path.exists() && self.key_path.exists() {
            log::info!("TLS certificates already exist, loading from disk");
            let cert_pem = fs::read_to_string(&self.cert_path)?;
            let key_pem = fs::read_to_string(&self.key_path)?;
            return Ok((cert_pem, key_pem));
        }
        
        log::info!("Generating new TLS certificates");
        
        // Generate certificate valid for localhost and local IPs (LND compatibility)
        let subject_alt_names = vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ];
        
        let rcgen::CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        
        // Write certificates 
        fs::write(&self.cert_path, &cert_pem)?;
        fs::write(&self.key_path, &key_pem)?;
        
        log::info!("TLS certificates generated and saved to {:?} and {:?}", 
                  self.cert_path, self.key_path);
        
        Ok((cert_pem, key_pem))
    }
}