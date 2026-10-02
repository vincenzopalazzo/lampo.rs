//! Extensions: protocol pieces that live outside the daemon. An extension
//! owns some custom peer message types, advertises feature bits, may ask the
//! node to keep a peer connected, may vouch for a fee a counterparty skims
//! from a payment, and may serve RPC methods. The daemon routes to it by
//! message type and method name and never needs to know what it is.

use std::net::{SocketAddr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::Arc;

use bitcoin::secp256k1::PublicKey;
use lightning::io;
use lightning::ln::msgs::{Init, LightningError};
use lightning::ln::wire::Type;
use lightning::types::features::InitFeatures;
use lightning::types::payment::PaymentHash;
use lightning::util::ser::{Writeable, Writer};

use crate::async_trait;
use crate::handler::Handler;
use crate::json;
use crate::jsonrpc;
use crate::signer::LampoSigner;
use crate::types::LampoChannel;

/// A custom peer message as it travels on the wire: the type id and the
/// payload after it. Decoding is the owning extension's business.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCustomMessage {
    pub type_id: u16,
    pub payload: Vec<u8>,
}

impl Type for RawCustomMessage {
    fn type_id(&self) -> u16 {
        self.type_id
    }
}

impl Writeable for RawCustomMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        w.write_all(&self.payload)
    }
}

/// A peer the node dials at startup and redials while it is gone, written
/// `NODE_ID@HOST:PORT` in configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistentPeer {
    pub node_id: PublicKey,
    /// Host name or IP literal, without brackets.
    pub host: String,
    pub port: u16,
}

impl PersistentPeer {
    /// Resolve `host:port` into socket addresses to dial.
    pub fn socket_addrs(&self) -> anyhow::Result<Vec<SocketAddr>> {
        let addrs: Vec<SocketAddr> = (self.host.as_str(), self.port).to_socket_addrs()?.collect();
        if addrs.is_empty() {
            anyhow::bail!("peer host `{}` did not resolve", self.host);
        }
        Ok(addrs)
    }
}

impl FromStr for PersistentPeer {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let (node_id, addr) = raw
            .trim()
            .split_once('@')
            .ok_or_else(|| anyhow::anyhow!("invalid peer `{raw}`: expected NODE_ID@HOST:PORT"))?;
        let node_id = PublicKey::from_str(node_id.trim())
            .map_err(|err| anyhow::anyhow!("invalid peer node id `{node_id}`: {err}"))?;
        let (host, port) = addr
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid peer address `{addr}`: expected HOST:PORT"))?;
        let port: u16 = port
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid peer port `{port}`"))?;
        if port == 0 {
            anyhow::bail!("peer port must be between 1 and 65535");
        }
        let host = host.trim().trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            anyhow::bail!("invalid peer address `{addr}`: empty host");
        }
        Ok(Self {
            node_id,
            host: host.to_owned(),
            port,
        })
    }
}

impl std::fmt::Display for PersistentPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "{}@[{}]:{}", self.node_id, self.host, self.port)
        } else {
            write!(f, "{}@{}:{}", self.node_id, self.host, self.port)
        }
    }
}

/// What the daemon hands an extension once the node is built.
#[derive(Clone)]
pub struct ExtensionContext {
    /// The LDK channel manager: channels, their config, offers.
    pub channel_manager: Arc<LampoChannel>,
    /// The node's signer, for blinded paths and entropy.
    pub signer: Arc<dyn LampoSigner>,
    /// The event bus: emit, and subscribe to every node event.
    pub events: Arc<dyn Handler>,
    /// Push queued custom messages to the sockets now, instead of at the
    /// next timer tick. Never call it from inside a peer callback.
    pub flush: Arc<dyn Fn() + Send + Sync>,
}

/// A protocol extension the daemon routes custom peer messages to.
///
/// Every peer callback runs while the peer manager holds its own locks, so
/// an extension must not call back into the peer manager from them; it
/// queues outbound messages and the daemon drains them.
#[async_trait]
pub trait CustomMessageExtension: Send + Sync {
    /// Short name for logs.
    fn name(&self) -> &'static str;

    /// Message type ids this extension owns. Two extensions may not share
    /// one, and an even type is only ever accepted when someone owns it.
    fn message_types(&self) -> Vec<u16>;

    /// Init feature bits to advertise to `peer`, OR'd with the node's own.
    fn init_features(&self, _peer: &PublicKey) -> InitFeatures {
        InitFeatures::empty()
    }

    /// Peers to dial at startup and redial while they are gone.
    fn persistent_peers(&self) -> Vec<PersistentPeer> {
        Vec::new()
    }

    /// A message of one of [`Self::message_types`] arrived from `sender`.
    fn handle_message(
        &self,
        msg: RawCustomMessage,
        sender: PublicKey,
    ) -> Result<(), LightningError>;

    /// Messages queued for sending. A message to a peer that is not
    /// connected is dropped by the peer manager.
    fn drain_outbound(&self) -> Vec<(PublicKey, RawCustomMessage)> {
        Vec::new()
    }

    /// A peer completed its handshake; `init` is what it sent. `Err(())`
    /// disconnects the peer at once (LDK's own semantics, e.g. because it
    /// lacks a feature this extension cannot do without), in which case
    /// [`Self::peer_disconnected`] is not called for it.
    fn peer_connected(&self, _peer: PublicKey, _init: &Init, _inbound: bool) -> Result<(), ()> {
        Ok(())
    }

    fn peer_disconnected(&self, _peer: PublicKey) {}

    /// Called once, after the node is built and before it listens.
    fn attach(&self, _context: ExtensionContext) {}

    /// How much, in msat, the previous hops may have skimmed from a payment
    /// with `payment_hash` and still have it claimed. `counterparties` has
    /// one entry per HTLC part, `None` when its channel is unknown. Zero
    /// unless the extension vouches for the fee.
    fn counterparty_skim_budget_msat(
        &self,
        _payment_hash: &PaymentHash,
        _counterparties: &[Option<PublicKey>],
    ) -> u64 {
        0
    }

    /// Serve `method`, or `Ok(None)` when it is not one of this
    /// extension's.
    async fn rpc(
        &self,
        _method: &str,
        _args: &json::Value,
    ) -> Result<Option<json::Value>, jsonrpc::Error> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: &str = "03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134";

    #[test]
    fn raw_message_writes_only_its_payload() {
        let msg = RawCustomMessage {
            type_id: 41041,
            payload: vec![1, 2, 3],
        };
        assert_eq!(msg.type_id(), 41041);
        assert_eq!(msg.encode(), vec![1, 2, 3]);
    }

    #[test]
    fn persistent_peer_round_trips_and_rejects_garbage() {
        let raw = format!("{NODE}@13.248.222.197:9735");
        let peer = PersistentPeer::from_str(&raw).unwrap();
        assert_eq!(peer.host, "13.248.222.197");
        assert_eq!(peer.port, 9735);
        assert_eq!(peer.to_string(), raw);

        let ipv6 = PersistentPeer::from_str(&format!("{NODE}@[::1]:9735")).unwrap();
        assert_eq!(ipv6.host, "::1");
        assert!(ipv6.to_string().ends_with("@[::1]:9735"));

        assert!(PersistentPeer::from_str("nonsense").is_err());
        assert!(PersistentPeer::from_str("00@127.0.0.1:9735").is_err());
        assert!(PersistentPeer::from_str(&format!("{NODE}@127.0.0.1:0")).is_err());
        assert!(PersistentPeer::from_str(&format!("{NODE}@127.0.0.1")).is_err());
    }
}
