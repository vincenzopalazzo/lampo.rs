mod remote_signer;
mod signer_adapter;

use std::fs;
use std::str::FromStr;
use std::sync::Arc;

use lightning_signer::lightning::sign::SignerProvider;
use lightning_signer::node::{NodeConfig, NodeServices};
use lightning_signer::persist::fs::FileSeedPersister;
use lightning_signer::policy::simple_validator::SimpleValidatorFactory;
use lightning_signer::signer::derive::KeyDerivationStyle;
use lightning_signer::signer::multi_signer::MultiSigner;
use lightning_signer::signer::ClockStartingTimeFactory;
use lightning_signer::util::clock::StandardClock;
use lightning_signer::util::loopback::LoopbackSignerKeysInterface;
use remote_signer::build_remote;
use triggered::Listener;
use vls_persist::kvv::redb::RedbKVVStore;
use vls_persist::kvv::{JsonFormat, KVVPersister};
use vls_proxy::nodefront::SignerFront;
use vls_proxy::vls_frontend::frontend::FileSourceFactory;
use vls_proxy::vls_frontend::Frontend;
use vls_proxy::vls_protocol_client::{DynSigner, SpendableKeysInterface};

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::bitcoin::{Address, ScriptBuf};
use lampo_common::conf::LampoConf;
use lampo_common::error;

use crate::signer_adapter::Adapter;

pub type InnerSpendableInterface = Arc<dyn SpendableKeysInterface<EcdsaSigner = DynSigner>>;

pub enum VLSSignerKind {
    Remote,
    InProcess,
}

/// VLS Key Manager Builder - This is ensuring that given a
/// VLS signer kind,  we are able to create the signer type,
/// but at the same time hiding the complexity of the code
/// under a builder patter.
pub struct VLSKeyManagerBuilder {
    kind: VLSSignerKind,
    config: Arc<LampoConf>,
}

impl VLSKeyManagerBuilder {
    pub fn new(kind: VLSSignerKind, config: Arc<LampoConf>) -> error::Result<Self> {
        Ok(Self { kind, config })
    }

    pub async fn build(self, listner: Listener) -> error::Result<InnerSpendableInterface> {
        let signer: InnerSpendableInterface = match self.kind {
            VLSSignerKind::InProcess => build_in_process(self.config, listner).await?,
            VLSSignerKind::Remote => build_remote(self.config).await?,
        };
        Ok(signer)
    }
}

async fn build_in_process(
    config: Arc<LampoConf>,
    listner: Listener,
) -> error::Result<InnerSpendableInterface> {
    let node_id_path = format!("{}/node_id", config.path());
    let signer_path = format!("{}/signer", config.path());
    let persister = RedbKVVStore::new(&signer_path);
    let persister = Arc::new(KVVPersister(persister, JsonFormat));
    let seed_persister = Arc::new(FileSeedPersister::new(&signer_path));
    let validator_factory = Arc::new(SimpleValidatorFactory::new());
    let starting_time_factory = ClockStartingTimeFactory::new();
    let clock = Arc::new(StandardClock());
    let services = NodeServices {
        validator_factory,
        starting_time_factory,
        persister,
        clock,
        trusted_oracle_pubkeys: Vec::new(),
    };
    // FIXME use Node directly - requires rework of LoopbackSignerKeysInterface in
    // the rls crate
    let signer = Arc::new(MultiSigner::new(services));

    let source_factory = Arc::new(FileSourceFactory::new(config.path(), config.network));
    let frontend = Frontend::new(
        Arc::new(SignerFront {
            signer: Arc::clone(&signer),
            external_persist: None,
        }),
        source_factory,
        // FIXME: pass down the bitcoin URL
        url::Url::parse("").unwrap(),
        listner,
    );
    frontend.start();

    let signer = if let Ok(node_id_hex) = fs::read_to_string(node_id_path.clone()) {
        let node_id = PublicKey::from_str(&node_id_hex)?;
        assert!(signer.get_node(&node_id).is_ok());

        let manager = LoopbackSignerKeysInterface { node_id, signer };

        // FIXME: change when we generate a sweep address
        let shutdown_scriptpubkey: ScriptBuf = manager.get_shutdown_scriptpubkey().unwrap().into();
        let shutdown_address = Address::from_script(&shutdown_scriptpubkey, config.network)
            .expect("shutdown script must be convertible to address");
        Adapter {
            inner: manager,
            sweep_address: shutdown_address,
        }
    } else {
        let node_config = NodeConfig {
            network: config.network,
            key_derivation_style: KeyDerivationStyle::Ldk,
            use_checkpoints: true,
            allow_deep_reorgs: false,
        };
        let (node_id, _seed) = signer.new_node(node_config, seed_persister).unwrap();
        fs::write(node_id_path, node_id.to_string()).expect("write node_id");
        let node = signer.get_node(&node_id).unwrap();

        let manager = LoopbackSignerKeysInterface { node_id, signer };

        let shutdown_scriptpubkey: ScriptBuf = manager.get_shutdown_scriptpubkey().unwrap().into();
        let shutdown_address = Address::from_script(&shutdown_scriptpubkey, config.network)
            .expect("shutdown script must be convertible to address");
        log::info!(
            "adding shutdown address {} to allowlist for {}",
            shutdown_address,
            hex::encode(&node_id.serialize())
        );
        node.add_allowlist(&vec![shutdown_address.to_string()])
            .expect("add to allowlist");

        Adapter {
            inner: manager,
            // FIXME: add a real sweep address
            sweep_address: shutdown_address,
        }
    };
    Ok(Arc::new(signer))
}
