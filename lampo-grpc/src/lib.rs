use std::sync::Arc;
use tonic::transport::Server;
use lampod::LampoDaemon;

pub const FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("descriptor");

mod server;

use server::lnrpc::lightning_server::LightningServer;
use server::GrpcServer;

pub async fn run_grpc(daemon: Arc<LampoDaemon>, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let server = GrpcServer { daemon };
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .build()
        .unwrap();
    log::info!("[gRPC] Server listening on {}", addr);

    Server::builder()
        .add_service(LightningServer::new(server))
        .add_service(reflection_service)
        .serve(addr.parse()?)
        .await?;

    Ok(())
}