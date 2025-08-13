use std::sync::Arc;
use std::path::Path;
use tonic::transport::{Identity, ServerTlsConfig};
use lampod::LampoDaemon;

use crate::auth::{MacaroonManager, TlsManager};
use crate::services::lightning::LightningService;
use crate::services::lightning::lnrpc::lightning_server::LightningServer;

pub struct LampoGrpcServer {
    daemon: Arc<LampoDaemon>,
    tls_manager: TlsManager,
    macaroon_manager: Arc<MacaroonManager>,
}

impl LampoGrpcServer {
    pub async fn new(daemon: Arc<LampoDaemon>) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir_path = daemon.conf().path();
        let data_dir = Path::new(&data_dir_path);
        
        let tls_manager = TlsManager::new(data_dir);
        let macaroon_manager = Arc::new(MacaroonManager::new(data_dir)?);
        
        // Ensure certificates exist
        tls_manager.ensure_certificates()?;
        
        log::info!("gRPC server initialized with LND compatibility");
        log::info!("TLS cert: {}/tls.cert", data_dir.display());
        log::info!("Admin macaroon: {}/admin.macaroon", data_dir.display());
        log::info!("Readonly macaroon: {}/readonly.macaroon", data_dir.display());
        
        Ok(Self {
            daemon,
            tls_manager,
            macaroon_manager,
        })
    }
    
    pub fn tls_config(&self) -> Result<ServerTlsConfig, Box<dyn std::error::Error>> {
        let (cert_pem, key_pem) = self.tls_manager.ensure_certificates()?;
        let identity = Identity::from_pem(cert_pem, key_pem);
        Ok(ServerTlsConfig::new().identity(identity))
    }
    
    pub fn lightning_service(&self) -> LightningServer<LightningService> {
        let service = LightningService::new(
            self.daemon.clone(),
            self.macaroon_manager.clone(),
        );
        LightningServer::new(service)
    }
}