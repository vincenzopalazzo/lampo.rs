//! Integration tests against a self hosted Spark regtest.
//!
//! These need the operator stack from `buildonspark/spark` running:
//!
//! ```text
//! cd spark && docker compose up -d
//! ```
//!
//! They are `#[ignore]`d so a plain `cargo test` stays hermetic; run
//! them with `cargo test -- --ignored` once the stack is up. This is
//! not a skipped failure: without operators there is nothing to talk
//! to, and a test that silently passed in that case would be lying.
use std::sync::Arc;

use spark::signer::{DefaultSigner, SparkSignerAdapter};
use spark_wallet::{Network, OperatorPoolConfig, SparkWallet, SparkWalletConfig, WalletBuilder};

/// The operators `docker compose up` starts, with the identity keys
/// baked into that repo's `docker/config.json`.
const OPERATORS: [(usize, &str, &str); 3] = [
    (
        0,
        "https://localhost:8535",
        "0322ca18fc489ae25418a0e768273c2c61cabb823edfb14feb891e9bec62016510",
    ),
    (
        1,
        "https://localhost:8536",
        "0341727a6c41b168f07eb50865ab8c397a53c7eef628ac1020956b705e43b6cb27",
    ),
    (
        2,
        "https://localhost:8537",
        "0305ab8d485cc752394de4981f8a5ae004f2becfea6f432c9a59d5022d8764f0a6",
    ),
];

/// Where the certs were copied out of the containers to.
const TLS_DIR: &str = "/tmp/spark-tls";

fn local_config() -> SparkWalletConfig {
    let mut config = SparkWalletConfig::default_config(Network::Regtest);
    let operators = OPERATORS
        .iter()
        .map(|(id, address, identity)| {
            // The trust anchor is the local CA, not the leaf: rustls
            // rejects a self signed cert presented as both.
            let cert = std::fs::read(format!("{TLS_DIR}/ca.crt"))
                .unwrap_or_else(|err| panic!("missing {TLS_DIR}/ca.crt: {err}"));
            let _ = id;
            SparkWalletConfig::create_operator_config(
                *id,
                &format!("{:064x}", id + 1),
                address,
                Some(&cert),
                identity,
            )
            .expect("operator config")
        })
        .collect::<Vec<_>>();
    config.operator_pool = OperatorPoolConfig::new(0, operators).expect("operator pool");
    config.validate().expect("config validates");
    config
}

async fn wallet(seed: [u8; 32]) -> SparkWallet {
    let signer = Arc::new(SparkSignerAdapter::new(Arc::new(
        DefaultSigner::new(&seed, Network::Regtest).expect("signer"),
    )));
    WalletBuilder::new(local_config(), signer)
        .build()
        .await
        .expect("the wallet must reach the local operators")
}

/// The load bearing question for the whole swap design: a wallet can
/// be built and used against operators alone, with no Spark service
/// provider in the picture. The swap daemon is its own provider, so if
/// this needs an SSP the architecture does not hold.
#[tokio::test]
#[ignore = "needs the spark operator stack, see the module docs"]
async fn wallet_connects_to_local_operators_without_an_ssp() {
    let wallet = wallet([7u8; 32]).await;

    let address = wallet.get_spark_address().expect("spark address");
    let address = address.to_address_string().expect("address encodes");
    assert!(
        address.starts_with("sp"),
        "expected a spark address, got `{address}`"
    );

    let balance = wallet.get_balance().await.expect("balance");
    assert_eq!(balance, 0, "a fresh wallet holds nothing");

    let claimable = wallet
        .list_claimable_htlc_transfers(None)
        .await
        .expect("querying htlcs must work: the swap engine polls this");
    assert!(claimable.is_empty());
}

/// Two wallets on the same local network must see different
/// identities, which is what makes them usable as swap counterparties.
#[tokio::test]
#[ignore = "needs the spark operator stack, see the module docs"]
async fn two_wallets_have_distinct_identities() {
    let one = wallet([1u8; 32]).await;
    let two = wallet([2u8; 32]).await;
    let one = one
        .get_spark_address()
        .unwrap()
        .to_address_string()
        .unwrap();
    let two = two
        .get_spark_address()
        .unwrap()
        .to_address_string()
        .unwrap();
    assert_ne!(one, two);
}
