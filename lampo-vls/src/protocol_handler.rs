use std::sync::Arc;

use lightning_signer::node::NodeServices;
use lightning_signer::persist::DummyPersister;
use lightning_signer::policy::simple_validator::make_simple_policy;
use lightning_signer::policy::simple_validator::SimpleValidatorFactory;
use lightning_signer::signer::ClockStartingTimeFactory;
use lightning_signer::util::clock::StandardClock;
use vls_proxy::grpc::adapter::ChannelRequest;
use vls_proxy::grpc::adapter::ClientId;
use vls_proxy::vls_protocol_client::ClientResult;
use vls_proxy::vls_protocol_client::{Error, Transport};
use vls_proxy::vls_protocol_signer::handler::Handler;
use vls_proxy::vls_protocol_signer::handler::{HandlerBuilder, RootHandler};
use vls_proxy::vls_protocol_signer::vls_protocol::serde_bolt::Array;
use vls_proxy::vls_protocol_signer::vls_protocol::serde_bolt::WireString;
use vls_proxy::vls_protocol_signer::vls_protocol::{model::PubKey, msgs};
use tokio::sync::oneshot;
use tokio::sync::mpsc::Sender;


use lampo_common::bitcoin::Network;

use crate::util::AsyncRuntime;

#[allow(dead_code)]
/// The `LampoVLSInProcess` represents a VLS client with a Null Transport.
/// Designed to run VLS in-process, but still performs the VLS protocol, No Persistence.
pub struct InProcessProtocolHandler {
    pub handler: RootHandler
}

/// Describe method to handle messages using the VLS protocol for Singer and Channel API.
impl Transport for InProcessProtocolHandler {
    /// Perform a call for the Signer Protocol API
    fn node_call(&self, msg: Vec<u8>) -> Result<Vec<u8>, Error> {
        // Deserialize the incoming message
        let message = msgs::from_vec(msg)?;
        // Handle the message using RootHandler
        let (result, _) = self.handler.handle(message).map_err(|_| Error::Transport)?;
        Ok(result.as_vec())
    }

    // Perform a call for the Channel Protocol API
    fn call(&self, db_id: u64, peer_id: PubKey, msg: Vec<u8>) -> Result<Vec<u8>, Error> {
        // Deserialize the incoming message
        let message = msgs::from_vec(msg)?;
        // Creating a ChannelHandler
        let handler = self.handler.for_new_client(0, peer_id, db_id);
        // Handle the message using ChannelHandler
        let (result, _) = handler.handle(message).map_err(|_| Error::Transport)?;
        Ok(result.as_vec())
    }
}

#[allow(dead_code)]
impl InProcessProtocolHandler {
    // Initialize the ProtocolHandler with Default Configuration, No Persistence
    pub fn new(network: Network, seed: &[u8; 32]) -> Self {
        // Create a dummy persister (no persistence)
        let persister = Arc::new(DummyPersister);
        // Define an allowlist with the given address
        let allowlist = vec![];
        // Create a simple policy for the network
        let policy = make_simple_policy(network);
        // Create Validators using SimpleValidatorFactory with the policy
        let validator_factory = Arc::new(SimpleValidatorFactory::new_with_policy(policy));
        let starting_time_factory = ClockStartingTimeFactory::new();
        let clock = Arc::new(StandardClock());
        let services = NodeServices {
            validator_factory,
            starting_time_factory,
            persister,
            clock,
        };
        let builder = HandlerBuilder::new(network, 0, services, seed.to_owned()).allowlist(allowlist);
        let (mut init_handler, _) = builder.build().expect("Cannot Build Root Handler");

		let preinit = msgs::HsmdDevPreinit {
			derivation_style: 1,
			network_name: WireString(network.to_string().into_bytes()),
			seed: None,
			allowlist: Array(vec![]),
		};
		let init = msgs::HsmdInit2 {
			derivation_style: 1,
			network_name: WireString(network.to_string().into_bytes()),
			dev_seed: None,
			dev_allowlist: Array(vec![]),
		};

		init_handler.handle(msgs::Message::HsmdDevPreinit(preinit)).expect("HSMD preinit failed");
		init_handler.handle(msgs::Message::HsmdInit2(init)).expect("HSMD init failed");

        let root_handler = init_handler.into_root_handler();
        InProcessProtocolHandler {
            handler: root_handler,
        }
    }
}

pub struct GrpcProtocolHandler {
	sender: Sender<ChannelRequest>,
	async_runtime: Arc<AsyncRuntime>,
}

impl GrpcProtocolHandler {
	pub async fn new(sender: Sender<ChannelRequest>, async_runtime: Arc<AsyncRuntime>) -> ClientResult<Self> {
        Ok(Self { sender, async_runtime })
	}

	async fn do_call_async(
		sender: Sender<ChannelRequest>, message: Vec<u8>, client_id: Option<ClientId>,
	) -> ClientResult<Vec<u8>> {
		let (reply_tx, reply_rx) = oneshot::channel();

		let request = ChannelRequest { client_id, message, reply_tx };

		// This can fail if gRPC adapter shut down
		sender.send(request).await.map_err(|_| Error::Transport)?;
		let reply = reply_rx.await.map_err(|_| Error::Transport)?;
		Ok(reply.reply)
	}
}

impl Transport for GrpcProtocolHandler {
	fn node_call(&self, message: Vec<u8>) -> ClientResult<Vec<u8>> {
        tokio::task::block_in_place(|| {
            self.async_runtime.block_on(GrpcProtocolHandler::do_call_async(self.sender.clone(), message, None))
        })
	}

	fn call(&self, dbid: u64, peer_id: PubKey, message: Vec<u8>) -> ClientResult<Vec<u8>> {
		let client_id = Some(ClientId { peer_id: peer_id.0, dbid });
        tokio::task::block_in_place(|| {
            self.async_runtime.block_on(GrpcProtocolHandler::do_call_async(self.sender.clone(), message, client_id))
        })
	}
}

