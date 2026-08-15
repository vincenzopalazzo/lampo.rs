use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk;
use lampo_common::ldk::ln::peer_handler::MessageHandler;
use lampo_common::ldk::ln::peer_handler::{IgnoringMessageHandler, PeerManager};
use lampo_common::ldk::net;
use lampo_common::ldk::net::SocketDescriptor;
use lampo_common::ldk::onion_message::messenger::{DefaultMessageRouter, OnionMessenger};
use lampo_common::ldk::routing::gossip::{NetworkGraph, P2PGossipSync};
use lampo_common::ldk::sign::EntropySource;
use lampo_common::types::NodeId;
use lampo_common::types::{LampoArcChannelManager, LampoChainMonitor, LampoGraph};

use crate::async_run;
use crate::chain::{LampoChainManager, WalletManager};
use crate::ln::LampoChannelManager;
use crate::utils::logger::LampoLogger;

pub type LampoArcOnionMessenger<L> = OnionMessenger<
    Arc<LampoKeysManager>,
    Arc<LampoKeysManager>,
    Arc<L>,
    Arc<LampoArcChannelManager<LampoChainMonitor, L>>,
    Arc<DefaultMessageRouter<Arc<LampoGraph>, Arc<L>, Arc<LampoKeysManager>>>,
    Arc<LampoArcChannelManager<LampoChainMonitor, L>>,
    IgnoringMessageHandler,
    IgnoringMessageHandler,
    IgnoringMessageHandler,
>;

pub type SimpleArcPeerManager<M, T, L> = PeerManager<
    SocketDescriptor,
    Arc<LampoArcChannelManager<M, L>>,
    Arc<P2PGossipSync<Arc<NetworkGraph<Arc<L>>>, Arc<T>, Arc<L>>>,
    Arc<LampoArcOnionMessenger<L>>,
    Arc<L>,
    IgnoringMessageHandler,
    Arc<LampoKeysManager>,
    IgnoringMessageHandler,
>;

type InnerLampoPeerManager =
    SimpleArcPeerManager<LampoChainMonitor, LampoChainManager, LampoLogger>;

/// Upper bound on the number of outbound dials the user-facing `connect`
/// path keeps in flight at once. Each in-flight dial pins a task, two file
/// descriptors and an LDK peer entry until the handshake reaper frees it, so
/// an unauthenticated flood would otherwise exhaust the process fd table.
const MAX_CONCURRENT_DIALS: usize = 32;

/// How long a single user-facing dial may run before it is abandoned. Mirrors
/// the reconnect loop's 5 s precedent, scaled to LDK's handshake-reaper bound
/// (two ping-timer intervals), so a silent/black-holed peer cannot pin the
/// request forever.
const DIAL_TIMEOUT: Duration = Duration::from_secs(20);

pub struct LampoPeerManager {
    peer_manager: Option<Arc<InnerLampoPeerManager>>,
    channel_manager: Option<Arc<LampoChannelManager>>,
    conf: LampoConf,
    logger: Arc<LampoLogger>,
    onion_messenger: Option<Arc<LampoArcOnionMessenger<LampoLogger>>>,
    /// Caps concurrent outbound dials started through `connect` (see
    /// [`MAX_CONCURRENT_DIALS`]).
    dial_permits: Arc<tokio::sync::Semaphore>,
    /// Serializes outbound dials per node id so a timed-out attempt's
    /// cleanup cannot race another dial to the same peer.
    outbound_dial_locks: Arc<Mutex<HashMap<NodeId, Arc<tokio::sync::Mutex<()>>>>>,
}

impl LampoPeerManager {
    pub fn new(conf: &LampoConf, logger: Arc<LampoLogger>) -> LampoPeerManager {
        LampoPeerManager {
            peer_manager: None,
            conf: conf.to_owned(),
            logger,
            channel_manager: None,
            onion_messenger: None,
            dial_permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DIALS)),
            outbound_dial_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn outbound_dial_lock(&self, node_id: NodeId) -> OutboundDialLockGuard {
        OutboundDialLockGuard::acquire(&self.outbound_dial_locks, node_id)
    }

    pub fn manager(&self) -> Arc<InnerLampoPeerManager> {
        self.peer_manager.clone().unwrap()
    }

    pub fn onion_messager(&self) -> Arc<LampoArcOnionMessenger<LampoLogger>> {
        self.onion_messenger.clone().unwrap()
    }

    pub fn init(
        &mut self,
        _onchain_manager: Arc<LampoChainManager>,
        wallet_manager: Arc<dyn WalletManager>,
        channel_manager: Arc<LampoChannelManager>,
    ) -> error::Result<()> {
        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let keys = wallet_manager.ldk_keys().keys_manager.clone();
        // LDK derives every per-connection BOLT-8 noise ephemeral key from
        // these bytes, and `PeerManager::new` documents them as
        // "cryptographically secure random bytes". A constant seed makes
        // `midstate = SHA256(seed)` public, so every handshake ephemeral is
        // publicly derivable from the cleartext pubkeys in Acts One/Two --
        // which voids Noise_XK confidentiality and authentication (an active
        // attacker can forge Act Two toward an initiator or Act Three toward a
        // responder under any node_id). Seed from the node's CSPRNG instead.
        let ephemeral_bytes = keys.get_secure_random_bytes();
        let graph = channel_manager.graph();
        let onion_messenger = Arc::new(OnionMessenger::new(
            keys.clone(),
            keys.clone(),
            self.logger.clone(),
            // ChannelManager implements NodeIdLookUp; use it (not EmptyNodeIdLookUp)
            // so the messenger can resolve hops when advancing offer blinded paths.
            channel_manager.manager(),
            Arc::new(DefaultMessageRouter::new(graph.clone(), keys.clone())),
            channel_manager.manager(), // Use channel manager for offers message handler
            IgnoringMessageHandler {}, // async_payments_message_handler
            IgnoringMessageHandler {}, // custom_onion_message_handler
            IgnoringMessageHandler {}, // custom_onion_message_contents
        ));

        let gossip_sync = Arc::new(P2PGossipSync::new(
            graph.clone(),
            None::<Arc<LampoChainManager>>,
            self.logger.clone(),
        ));

        let lightning_msg_handler = MessageHandler {
            chan_handler: channel_manager.manager(),
            onion_message_handler: onion_messenger.clone(),
            route_handler: gossip_sync,
            custom_message_handler: IgnoringMessageHandler {},
            send_only_message_handler: IgnoringMessageHandler {},
        };

        let peer_manager = InnerLampoPeerManager::new(
            lightning_msg_handler,
            current_time.try_into().unwrap(),
            &ephemeral_bytes,
            channel_manager.logger.clone(),
            wallet_manager.ldk_keys().keys_manager.clone(),
        );
        self.peer_manager = Some(Arc::new(peer_manager));
        self.channel_manager = Some(channel_manager.clone());
        self.onion_messenger = Some(onion_messenger);
        Ok(())
    }

    /// Run the peer manager event loop without requiring an explicit shutdown flag.
    ///
    /// This method is kept for backward compatibility and delegates to
    /// [`run_with_shutdown`], creating an internal shutdown flag that is
    /// never triggered programmatically.
    pub fn run(&self) -> error::Result<()> {
        let shutdown = Arc::new(AtomicBool::new(false));
        self.run_with_shutdown(shutdown)
    }

    /// Run the peer manager event loop, allowing cooperative shutdown via the
    /// provided `shutdown` flag.
    pub fn run_with_shutdown(&self, shutdown: Arc<AtomicBool>) -> error::Result<()> {
        let listen_port = self.conf.port;
        let Some(ref peer_manager) = self.peer_manager else {
            error::bail!("peer manager is None, at this point this should be not None");
        };
        let peer_manager = peer_manager.clone();
        let chan_manager = self
            .channel_manager
            .clone()
            .ok_or(error::anyhow!("channel manager is None"))?;
        let alias = self.conf.alias.clone().unwrap_or_default();
        // The address we bind to and the address we *announce* are not the
        // same thing. Binding falls back to loopback so a node with no
        // configured address still comes up; announcing loopback would
        // publish an unreachable `127.0.0.1` to the gossip network, so we
        // only announce an address the operator explicitly configured --
        // matching `getinfo`, which reports `None` as "no advertised
        // address".
        let announce_addr = self.conf.announce_addr.clone();
        let bind_host = announce_addr
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let bind_addr = format!("{bind_host}:{listen_port}");

        // Bind the p2p listener eagerly, *before* detaching the accept loop.
        // The bind used to live inside the spawned task, so a failure (most
        // commonly the port being already taken) died in a dropped
        // `JoinHandle` -- unlogged and unobserved -- and the node came up
        // "healthy" (REST answering, chain syncing, getinfo echoing the
        // configured address, the log even claiming it was listening) with no
        // inbound peer plane at all: no inbound channels, no node-announcement
        // refresh, and no way for monitoring to notice. A Lightning node must
        // not run headless, so surface the error to the caller and refuse to
        // start instead.
        let listener = std::net::TcpListener::bind(&bind_addr)
            .map_err(|e| error::anyhow!("failed to bind LN p2p listener on `{bind_addr}`: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| error::anyhow!("failed to set the LN p2p listener non-blocking: {e}"))?;

        // Reconnect to channel counterparties. LDK never redials anyone:
        // after a restart (or any TCP drop) a node with live, ready channels
        // sits at zero peers forever -- it stops forwarding and cannot be
        // paid -- unless something above it re-establishes the connections.
        // `ConnectionNeeded` does not cover this: it only fires when LDK has
        // an onion message to deliver AND knows an address. This loop is
        // lampo's equivalent of ldk-node's PEER_RECONNECTION_INTERVAL task:
        // every tick, dial every disconnected channel counterparty at the
        // address we last reached them on. Announced peers are already
        // handled by `ConnectionNeeded`.
        {
            let peer_manager = peer_manager.clone();
            let chan_manager = chan_manager.clone();
            let shutdown = shutdown.clone();
            let store_path = peer_store_path(&self.conf);
            let dial_locks = Arc::clone(&self.outbound_dial_locks);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(10));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let known = load_peers(&store_path);
                    for channel in chan_manager.manager().list_channels() {
                        let peer = channel.counterparty.node_id;
                        if peer_manager.peer_by_node_id(&peer).is_some() {
                            continue;
                        }
                        let Some(host) = known
                            .get(&peer.to_string())
                            .and_then(|addr| addr.parse().ok())
                        else {
                            continue;
                        };
                        log::info!(
                            target: "lampo",
                            "reconnecting to channel peer `{peer}` at `{host}`"
                        );
                        // Share the per-node dial lock with user `/connect` so
                        // a reconnect timeout cannot node-wide-disconnect a
                        // concurrent user dial.
                        let node_lock = OutboundDialLockGuard::acquire(&dial_locks, peer);
                        let _node_guard = node_lock.lock.lock().await;
                        if tokio::time::timeout(
                            Duration::from_secs(5),
                            dial(peer_manager.clone(), peer, host),
                        )
                        .await
                        .is_err()
                            && peer_manager.peer_by_node_id(&peer).is_none()
                        {
                            peer_manager.disconnect_by_node_id(peer);
                        }
                    }
                }
            });
        }

        // Adopt the std listener into tokio *before* detaching the accept
        // loop. `from_std` only fails if the socket is still blocking; that
        // is a startup error and must surface from this function, not panic
        // inside a dropped JoinHandle and leave the node running headless.
        let listener = tokio::net::TcpListener::from_std(listener)
            .map_err(|e| error::anyhow!("failed to adopt LN p2p listener into tokio: {e}"))?;
        log::info!(target: "lampo", "Listening for in-bound connection on {bind_addr}");
        tokio::spawn(async move {
            // Node announcement runs in its own task, not inside each
            // inbound connection's task -- there is one announcement to
            // refresh, not one per peer, and the old per-connection
            // placement is what broke routing (the accept loop below used
            // to `.await` a per-connection task whose announcement loop
            // never returned, so the node accepted exactly one inbound
            // peer for its whole life). It is spawned here, only after the
            // listener is up and only when an address was configured, so a
            // failed bind never leaves it refreshing an unreachable
            // endpoint. Mirrors ldk-node, which keeps the two separate.
            if let Some(announce_host) = announce_addr.clone() {
                let socket_addr = format!("{announce_host}:{listen_port}");
                let peer_manager = peer_manager.clone();
                let chan_manager = chan_manager.clone();
                let shutdown = shutdown.clone();
                let alias = alias.clone();
                tokio::spawn(async move {
                    // Refresh about once a minute: fresh enough, without
                    // the gossip spam of the previous one-second tick.
                    let mut interval = tokio::time::interval(Duration::from_secs(60));
                    loop {
                        interval.tick().await;
                        if shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        // No point announcing without a public channel;
                        // peers drop such announcements anyway, and it may
                        // not propagate until the channel has 6+ confs.
                        if chan_manager
                            .manager()
                            .list_channels()
                            .iter()
                            .any(|chan| chan.is_announced)
                        {
                            peer_manager.broadcast_node_announcement(
                                [0; 3],
                                alias.as_bytes().try_into().unwrap_or([0u8; 32]),
                                vec![ldk::ln::msgs::SocketAddress::from_str(&socket_addr).expect(
                                    "impossible to convert an addr to ln socket addr (wire format)",
                                )],
                            );
                        }
                    }
                });
            }

            loop {
                if shutdown.load(Ordering::Acquire) {
                    log::info!(target: "lampo", "Peer manager shutting down");
                    break;
                }
                let peer_manager = peer_manager.clone();
                let shutdown = shutdown.clone();
                let accept = tokio::select! {
                    result = listener.accept() => {
                        result.map_err(|err| error::anyhow!("Error accepting connection: {}", err))?
                    }
                    _ = async {
                        while !shutdown.load(Ordering::Acquire) {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    } => {
                        log::info!(target: "lampo", "Peer manager shutting down");
                        break;
                    }
                };
                let (tcp_stream, _) = accept;
                log::info!(target: "lampo", "Got new connection {}", tcp_stream.peer_addr().unwrap());
                // Fire and forget: setup_inbound drives this connection on
                // its own tasks, so the accept loop must NOT await it --
                // awaiting it is exactly what wedged the loop after one
                // peer.
                tokio::spawn(async move {
                    net::setup_inbound(
                        peer_manager,
                        tcp_stream
                            .into_std()
                            .expect("impossible to convert a tcp_stream from tokio to std"),
                    )
                    .await;
                });
            }
            Ok::<(), error::Error>(())
        });
        Ok(())
    }

    pub fn is_connected_with(&self, peer_id: NodeId) -> bool {
        let Some(ref manager) = self.peer_manager else {
            panic!("at this point the peer manager should be known");
        };
        manager.peer_by_node_id(&peer_id).is_some()
    }

    pub async fn connect(&self, node_id: NodeId, host: SocketAddr) -> error::Result<()> {
        // Bound the resources an unauthenticated `/connect` caller can pin.
        // Without a cap, a request flood accumulates one task + two fds + one
        // LDK peer entry per in-flight dial until the handshake reaper frees
        // it, exhausting the fd table and taking down both the REST API and
        // the LN listener. Fail fast once the budget is spent instead of
        // queueing, and time each dial out near the reaper bound so a silent
        // peer cannot hold the slot for the full ping interval.
        // Take a global dial permit *before* waiting on the per-node lock so
        // silent-peer contention cannot queue unbounded HTTP tasks outside the
        // 32-slot cap. Serialize per node so overlapping dials cannot leave a
        // timeout's `disconnect_by_node_id` tearing down a sibling attempt that
        // already completed (or an inbound that landed for the same id).
        let _permit = Arc::clone(&self.dial_permits)
            .try_acquire_owned()
            .map_err(|_| error::anyhow!("too many concurrent connection attempts"))?;
        let node_lock = self.outbound_dial_lock(node_id);
        let _node_guard = node_lock.lock.lock().await;
        let result = match tokio::time::timeout(DIAL_TIMEOUT, dial(self.manager(), node_id, host))
            .await
        {
            Ok(result) => result,
            Err(_) => {
                // `timeout` only drops the close-notification future from
                // `connect_outbound`; the socket and I/O tasks stay registered
                // with the peer manager until its handshake reaper runs. Tear
                // them down before the semaphore permit is released so a flood
                // of timed-out `/connect`s cannot accumulate more than
                // `MAX_CONCURRENT_DIALS` live outbound connections.
                // Skip cleanup when a live peer already exists for this id —
                // an inbound (or a completed sibling dial) must not be killed
                // with the timed-out socket. `disconnect_by_node_id` is not
                // socket-scoped.
                if self.manager().peer_by_node_id(&node_id).is_none() {
                    self.manager().disconnect_by_node_id(node_id);
                } else {
                    log::debug!(
                        target: "lampo",
                        "dial to `{node_id}` timed out but peer is already connected; leaving it alone"
                    );
                }
                Err(error::anyhow!("timed out connecting to peer `{node_id}`"))
            }
        };
        drop(_node_guard);
        drop(node_lock);
        result?;
        // Remember where this peer lives. LDK does not redial anyone on
        // restart, and a peer that never announced an address -- the common
        // case for an unannounced node -- is unreachable through the network
        // graph, so this file is the only durable record of how to get back
        // to a channel counterparty. The reconnect loop reads it.
        remember_peer(&peer_store_path(&self.conf), &node_id, &host);
        Ok(())
    }

    pub async fn disconnect(&self, node_id: NodeId) -> error::Result<()> {
        //check the pubkey matches a valid connected peer
        if self.manager().peer_by_node_id(&node_id).is_none() {
            error::bail!("Error: Could not find peer `{node_id}`");
        }

        self.manager().disconnect_by_node_id(node_id);
        Ok(())
    }
}

/// Holds a per-node dial lock entry and removes it from the map on drop when
/// nothing else references it — including when the `/connect` future is
/// cancelled before reaching an explicit prune call.
struct OutboundDialLockGuard {
    node_id: NodeId,
    lock: Arc<tokio::sync::Mutex<()>>,
    map: Arc<Mutex<HashMap<NodeId, Arc<tokio::sync::Mutex<()>>>>>,
}

impl OutboundDialLockGuard {
    fn acquire(
        map: &Arc<Mutex<HashMap<NodeId, Arc<tokio::sync::Mutex<()>>>>>,
        node_id: NodeId,
    ) -> Self {
        let lock = {
            let mut map = map.lock().expect("outbound dial lock map poisoned");
            map.entry(node_id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        Self {
            node_id,
            lock,
            map: Arc::clone(map),
        }
    }
}

impl Drop for OutboundDialLockGuard {
    fn drop(&mut self) {
        let Ok(mut map) = self.map.lock() else {
            return;
        };
        if map.get(&self.node_id).is_some_and(|existing| {
            Arc::ptr_eq(existing, &self.lock) && Arc::strong_count(&self.lock) == 2
        }) {
            map.remove(&self.node_id);
        }
    }
}

/// Dial `host` and wait until the handshake completes. A close before
/// that is a failed connect, not success. Shared by user-driven
/// `connect` and the reconnect loop.
async fn dial(
    manager: Arc<InnerLampoPeerManager>,
    node_id: NodeId,
    host: SocketAddr,
) -> error::Result<()> {
    let Some(close_callback) = net::connect_outbound(manager.clone(), node_id, host).await else {
        log::warn!(target: "lampo", "impossible connect with the peer `{node_id}`");
        error::bail!("impossible connect with the peer `{node_id}`");
    };
    let mut connection_closed_future = Box::pin(close_callback);
    loop {
        match futures::poll!(&mut connection_closed_future) {
            std::task::Poll::Ready(_) => {
                log::info!(target: "lampo", "node `{node_id}` disconnected before handshake");
                error::bail!("peer `{node_id}` closed before handshake");
            }
            std::task::Poll::Pending => {}
        }
        // Avoid blocking the tokio context by sleeping a bit
        match manager.peer_by_node_id(&node_id) {
            Some(_) => return Ok(()),
            None => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

/// Where the last-known address of every peer we dialled is kept:
/// `<lampo dir>/peers.json`, a flat `node_id -> "host:port"` map.
fn peer_store_path(conf: &LampoConf) -> std::path::PathBuf {
    std::path::Path::new(&conf.path()).join("peers.json")
}

fn load_peers(path: &std::path::Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| lampo_common::json::from_str(&content).ok())
        .unwrap_or_default()
}

fn remember_peer(path: &std::path::Path, node_id: &NodeId, host: &SocketAddr) {
    let mut peers = load_peers(path);
    peers.insert(node_id.to_string(), host.to_string());
    match lampo_common::json::to_string_pretty(&peers) {
        Ok(json) => {
            if let Err(err) = std::fs::write(path, json) {
                log::warn!(target: "lampo", "failed to persist peer address: {err}");
            }
        }
        Err(err) => log::warn!(target: "lampo", "failed to serialize peer store: {err}"),
    }
}
