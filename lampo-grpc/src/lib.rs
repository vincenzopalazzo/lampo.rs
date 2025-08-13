mod auth;
mod server;
mod services;

pub use server::LampoGrpcServer;

use lampod::LampoDaemon;
use std::sync::Arc;
use tonic::transport::Server;

pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("descriptor");

pub async fn run_grpc(
    daemon: Arc<LampoDaemon>,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let grpc_server = server::LampoGrpcServer::new(daemon).await?;
    
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .build()
        .unwrap();
    
    log::info!("[gRPC] Server listening on {} with LND compatibility", addr);

    Server::builder()
        .tls_config(grpc_server.tls_config()?)?
        .add_service(grpc_server.lightning_service())
        .add_service(reflection_service)
        .serve(addr.parse()?)
        .await?;

    Ok(())
}