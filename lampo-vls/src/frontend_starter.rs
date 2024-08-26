use tokio::runtime::Runtime;
use vls_proxy::vls_frontend::Frontend;
use std::sync::Arc;

pub struct FrontendStarter;

impl FrontendStarter {
    pub fn start_frontend(frontend: Arc<Frontend>) {
        // Create a new Tokio runtime
        let rt = Runtime::new().expect("Failed to create Tokio runtime");

        // Spawn a thread to run the Tokio runtime
        std::thread::spawn(move || {
            rt.block_on(async {
                frontend.start();
                
                // Keep the runtime alive
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
            });
        });
    }
}
