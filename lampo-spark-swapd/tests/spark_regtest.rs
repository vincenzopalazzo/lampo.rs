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

// --- funding and the spark leg of a swap ---

const BITCOIND: &str = "spark-bitcoind-1";

/// Drive the regtest bitcoind the operator stack watches.
fn bitcoin_cli(args: &[&str]) -> String {
    let mut cmd = std::process::Command::new("docker");
    cmd.args([
        "exec",
        BITCOIND,
        "bitcoin-cli",
        "-regtest",
        "-rpcport=8332",
        "-rpcuser=testutil",
        "-rpcpassword=testutilpassword",
        "-rpcwallet=default",
    ])
    .args(args);
    let out = cmd.output().expect("docker exec bitcoin-cli");
    if !out.status.success() {
        panic!(
            "bitcoin-cli {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn mine(blocks: u32) {
    let address = bitcoin_cli(&["getnewaddress"]);
    bitcoin_cli(&["generatetoaddress", &blocks.to_string(), &address]);
}

/// Put `amount_sat` into `wallet` the way a user would: send to a
/// deposit address on chain, confirm it, then claim it into the tree.
/// This is operator-only work, no service provider involved.
async fn fund(wallet: &SparkWallet, amount_sat: u64) {
    use bitcoin::consensus::deserialize;
    use bitcoin::hex::FromHex;

    let deposit = wallet
        .generate_deposit_address()
        .await
        .expect("deposit address");
    let address = deposit.address.to_string();
    // Let the operators register and start watching the address before
    // the funding transaction appears in a block, otherwise the
    // chainwatcher has nothing to match it against.
    mine(1);
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    let btc = format!("{:.8}", amount_sat as f64 / 100_000_000.0);
    let txid = bitcoin_cli(&["sendtoaddress", &address, &btc]);
    // The operators only credit a deposit once it is buried.
    mine(6);
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    let raw = bitcoin_cli(&["getrawtransaction", &txid]);
    let tx: bitcoin::Transaction =
        deserialize(&Vec::<u8>::from_hex(&raw).expect("tx hex")).expect("tx decodes");
    let vout = tx
        .output
        .iter()
        .position(|out| out.script_pubkey == deposit.address.script_pubkey())
        .expect("the deposit output must be in the transaction") as u32;

    // The chainwatcher polls, so the deposit is not claimable instantly.
    let mut claimed = None;
    for _ in 0..30 {
        match wallet.claim_deposit(tx.clone(), vout).await {
            Ok(leaves) => {
                claimed = Some(leaves);
                break;
            }
            Err(err) => {
                let err = err.to_string();
                if err.contains("already used") {
                    panic!("the deposit address was consumed by a failed attempt: {err}");
                }
                log_wait(&format!("deposit not claimable yet: {err}"));
                mine(1);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
    assert!(claimed.is_some(), "the deposit was never claimable");
}

fn log_wait(message: &str) {
    println!("  ... {message}");
}

/// The spark half of a swap, end to end: one wallet locks funds behind
/// a payment hash, the other sees the pending htlc and takes it with
/// the preimage. This is exactly what the swap engine does once the
/// lightning leg reveals that preimage.
///
/// KNOWN BLOCKED. It does not reach the htlc: funding fails first,
/// with
///
/// ```text
/// bitcoin error: invalid transaction: signed tx input has empty witness
/// ```
///
/// which comes from `verify_finalized_taproot_signature` in the sdk,
/// not from us and not from the operators. The sdk checks the tx the
/// operator returns after server side FROST aggregation and wants a 64
/// byte schnorr signature in the witness; the operator returned none.
///
/// That is a version skew: breez/spark-sdk is pinned at `8c6abb1`
/// (2026-08-07) and the operators here were built from
/// buildonspark/spark `eaddc41` (2026-08-08), whose newest commit is
/// "bind deposit finalize decision to prepare", a change to exactly
/// this deposit signing handshake. Resolving it means pinning the
/// operators to a build the sdk pin was written against, or moving the
/// sdk pin forward, and neither is guesswork worth doing blind.
///
/// Kept as the reproduction: it names the wall the swap is behind.
#[tokio::test]
#[ignore = "blocked on an sdk/operator version skew, see the doc comment"]
async fn spark_htlc_is_locked_and_claimed_with_the_preimage() {
    use bitcoin::hashes::{sha256, Hash as _};

    let sender = wallet([21u8; 32]).await;
    let receiver = wallet([22u8; 32]).await;

    fund(&sender, 100_000).await;
    let sender_start = sender.get_balance().await.expect("sender balance");
    assert!(sender_start > 0, "funding must land before the swap");
    let receiver_start = receiver.get_balance().await.expect("receiver balance");

    let preimage = [42u8; 32];
    let payment_hash = sha256::Hash::hash(&preimage);
    let receiver_address = receiver.get_spark_address().expect("receiver address");

    let amount_sat = 10_000;
    let transfer = sender
        .create_htlc(
            amount_sat,
            &receiver_address,
            &payment_hash,
            std::time::Duration::from_secs(3600),
            None,
        )
        .await
        .expect("the htlc must be created");
    println!("locked htlc {} for {payment_hash}", transfer.id);

    // The receiver must be able to find it by polling, which is how the
    // swap engine learns its counterparty has locked up.
    let mut seen = false;
    for _ in 0..20 {
        let claimable = receiver
            .list_claimable_htlc_transfers(None)
            .await
            .expect("query htlcs");
        if claimable.iter().any(|transfer| {
            transfer
                .htlc_preimage_request
                .as_ref()
                .map(|request| request.payment_hash == payment_hash)
                .unwrap_or(false)
        }) {
            seen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(seen, "the receiver never saw the pending htlc");

    let preimage = spark::services::Preimage::from_hex(&hex_of(&preimage)).expect("preimage");
    receiver
        .claim_htlc(&preimage)
        .await
        .expect("the preimage must settle the htlc");

    let receiver_end = receiver.get_balance().await.expect("receiver balance");
    assert_eq!(
        receiver_end,
        receiver_start + amount_sat,
        "the claimed amount must land in the receiver's balance"
    );
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
