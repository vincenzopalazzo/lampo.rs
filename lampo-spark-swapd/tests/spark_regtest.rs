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

/// 32 fresh bytes from the OS. Operator state persists across runs in
/// the postgres volume, so wallet seeds and preimages must be unique
/// per run or a second run collides on an already-used payment hash.
fn nonce() -> [u8; 32] {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut bytes)
        .unwrap();
    bytes
}

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

    // Fund and claim while unconfirmed, then confirm -- the order the
    // sdk's own itest helper uses. The operators register the address
    // at generation time, so the deposit is claimable straight from
    // the mempool; pre-mining it instead moves it into on-chain
    // deposit processing and the claim path stops working.
    let btc = format!("{:.8}", amount_sat as f64 / 100_000_000.0);
    let txid = bitcoin_cli(&["sendtoaddress", &address, &btc]);

    let raw = bitcoin_cli(&["getrawtransaction", &txid]);
    let tx: bitcoin::Transaction =
        deserialize(&Vec::<u8>::from_hex(&raw).expect("tx hex")).expect("tx decodes");
    let vout = tx
        .output
        .iter()
        .position(|out| out.script_pubkey == deposit.address.script_pubkey())
        .expect("the deposit output must be in the transaction") as u32;

    let leaves = wallet
        .claim_deposit(tx, vout)
        .await
        .expect("the deposit must be claimable");
    assert!(!leaves.is_empty(), "claiming yielded no leaves");

    // A freshly claimed deposit leaf sits in "creating" status until the
    // operators confirm the funding tx on chain and move it to
    // available. Mine to confirm, then `sync` to pull the refreshed tree
    // rather than waiting on the event stream, which is the fallback the
    // sdk itself documents on `sync`.
    for _ in 0..30 {
        mine(1);
        wallet.sync().await.ok();
        if wallet.get_balance().await.unwrap_or(0) >= amount_sat {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("the claimed deposit never became spendable balance");
}

fn log_wait(message: &str) {
    println!("  ... {message}");
}

/// The spark half of a swap, end to end, against local operators: one
/// wallet is funded from the regtest chain, locks a hash-locked htlc,
/// and the other wallet claims it with the preimage. This is exactly
/// what the swap engine does once the lightning leg reveals that
/// preimage, and it is the first execution of any swap machinery
/// against a real Spark network.
///
/// Getting here required matching the operator build to the sdk pin
/// (see the README) and learning three things about the flow: a
/// deposit is claimed from the mempool and only becomes spendable
/// after a confirmation plus a `sync`; a deposit lands as one leaf, so
/// the whole balance is locked rather than a partial amount; and both
/// the funded balance and the claimed balance settle after a `sync`,
/// not instantly.
#[tokio::test]
#[ignore = "needs the spark operator stack, see the module docs"]
async fn spark_htlc_is_locked_and_claimed_with_the_preimage() {
    use bitcoin::hashes::{sha256, Hash as _};

    let mut sender_seed = nonce();
    let mut receiver_seed = nonce();
    // keep them distinct even in the astronomically unlikely tie
    sender_seed[0] ^= 0x01;
    receiver_seed[0] ^= 0x02;
    let sender = wallet(sender_seed).await;
    let receiver = wallet(receiver_seed).await;

    fund(&sender, 100_000).await;
    let sender_start = sender.get_balance().await.expect("sender balance");
    assert!(sender_start > 0, "funding must land before the swap");
    let receiver_start = receiver.get_balance().await.expect("receiver balance");

    let preimage = nonce();
    let payment_hash = sha256::Hash::hash(&preimage);
    let receiver_address = receiver.get_spark_address().expect("receiver address");

    // A deposit lands as a single leaf, and create_htlc cannot mint
    // change from it, so lock the whole balance. Splitting into partial
    // amounts is a leaf-optimization concern the swap does not need to
    // prove here.
    let amount_sat = sender_start;
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
        receiver.sync().await.ok();
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

    // The claimed leaf lands after a sync, same as any received transfer.
    let mut receiver_end = receiver_start;
    for _ in 0..15 {
        receiver.sync().await.ok();
        receiver_end = receiver.get_balance().await.expect("receiver balance");
        if receiver_end >= receiver_start + amount_sat {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert_eq!(
        receiver_end,
        receiver_start + amount_sat,
        "the claimed amount must land in the receiver's balance"
    );
    println!("swap complete: receiver now holds {receiver_end} sat");
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- the full swap: lightning leg + spark leg joined by one hash ---

use lampo_spark_swapd::engine::Engine;
use lampo_spark_swapd::lampo_leg::LampoLeg;
use lampo_spark_swapd::settings::Settings;
use lampo_spark_swapd::spark_leg::SparkLeg;
use lampo_spark_swapd::store::SwapStore;
use lampo_spark_swapd::swap::State;

/// Settings for an in-test engine. The spark side is unused here (the
/// legs are constructed directly), so only the swap knobs matter.
fn engine_settings(quote_expiry_secs: u64) -> Settings {
    Settings {
        spark_network: "regtest".to_owned(),
        spark_seed_file: std::path::PathBuf::from("/dev/null"),
        quote_expiry_secs,
        spark_htlc_expiry_secs: 3600,
        api_addr: "127.0.0.1:0".to_owned(),
        spark_operators: Vec::new(),
    }
}

/// Direction A, the whole thing: a Spark user pays a BOLT12 offer.
///
/// Two lampo nodes with a channel stand in for the swap node (S) and
/// the merchant issuing the offer (M); two spark wallets stand in for
/// the swap node and the paying user. The only value that crosses
/// between lightning and spark is the payment hash. The engine drives
/// it: fetch the offer, wait for the user's spark htlc on that hash,
/// pay M over lightning, and claim the user's htlc with the preimage
/// the payment revealed.
#[tokio::test]
#[ignore = "needs the spark operator stack and bitcoind, see the module docs"]
async fn direction_a_full_swap_spark_to_lightning() {
    use lampo_common::model::{request, response};
    use lampo_testing::prelude::*;
    use lampo_testing::LampoTesting;

    init_logger();

    // --- lightning side: swap node S with a channel to merchant M ---
    let node_s = LampoTesting::tmp().await.expect("node S");
    let node_m = std::sync::Arc::new(LampoTesting::new(node_s.btc.clone()).await.expect("node M"));
    node_s
        .fund_channel_with(node_m.clone(), 1_000_000)
        .await
        .expect("channel S -> M");

    // --- spark side: the swap node's wallet and the user's wallet ---
    let swapd_spark = std::sync::Arc::new(wallet(nonce()).await);
    let user_spark = wallet(nonce()).await;

    // The swap moves 50_000 sat == 50_000_000 msat across the two.
    let amount_sat: u64 = 50_000;
    let amount_msat = amount_sat * 1000;
    fund(&user_spark, amount_sat).await;

    // M issues the offer the user wants to pay.
    let offer: response::Offer = node_m
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some("swap me".to_owned()),
                amount_msat: Some(amount_msat),
            },
        )
        .await
        .expect("offer");

    // Build the engine on the swap node's lampo handler and spark wallet.
    let store_dir = std::env::temp_dir().join(format!("swapd-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store_dir);
    let engine = std::sync::Arc::new(Engine::new(
        LampoLeg::new(node_s.lampod()),
        SparkLeg::new(swapd_spark.clone()),
        SwapStore::new(store_dir).expect("store"),
        engine_settings(90),
    ));

    // Quote: fetch M's invoice through S, pin the payment hash, and get
    // the address the user must lock against.
    let quote = engine
        .quote_spark_to_ln(&offer.bolt12, None)
        .await
        .expect("quote");
    assert_eq!(quote.amount_msat, amount_msat);

    // The user locks their spark htlc on that hash, to the swap node.
    use bitcoin::hashes::sha256;
    use std::str::FromStr;
    let receiver = spark::address::SparkAddress::from_str(&quote.spark_address).expect("addr");
    let payment_hash = sha256::Hash::from_str(&quote.payment_hash).expect("hash");
    user_spark
        .create_htlc(
            amount_sat,
            &receiver,
            &payment_hash,
            std::time::Duration::from_secs(3600),
            None,
        )
        .await
        .expect("user locks the spark htlc");

    // Drive the engine and wait for the swap to settle. run() reconciles
    // on a tick: it sees the locked htlc, pays M, and claims the htlc.
    tokio::spawn(engine.clone().run());

    let mut final_state = None;
    for _ in 0..30 {
        if let Some(swap) = engine
            .list()
            .into_iter()
            .find(|s| s.payment_hash.as_deref() == Some(&quote.payment_hash))
        {
            if swap.is_terminal() {
                final_state = Some(swap.state);
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    assert_eq!(
        final_state,
        Some(State::Done),
        "the swap must complete; got {final_state:?}"
    );

    // The swap node now holds the user's spark funds...
    let mut swapd_balance = 0;
    for _ in 0..15 {
        swapd_spark.sync().await.ok();
        swapd_balance = swapd_spark.get_balance().await.unwrap_or(0);
        if swapd_balance >= amount_sat {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(
        swapd_balance >= amount_sat,
        "the swap node must hold the claimed spark funds, has {swapd_balance}"
    );

    // ...and the merchant was paid over lightning.
    println!(
        "full swap complete: swap node holds {swapd_balance} sat on spark, M paid over lightning"
    );
}

fn init_logger() {
    let _ = std::env::var("TEST_LOG_LEVEL")
        .ok()
        .and_then(|level| lampo_common::logger::init(&level, None).ok());
}

/// Direction B, the whole thing: a lightning payer funds a spark
/// address. The swap node publishes an offer mapped to the user's
/// spark address; the user pays it over lightning and learns the
/// preimage; the engine, seeing the payment, creates a spark htlc to
/// the user on the same hash; the user claims it with that preimage.
///
/// This is the trusted-window direction: the lightning leg settles
/// before the spark htlc exists, so the swap node is briefly paid
/// while still owing the htlc. The persisted swap record is what makes
/// it deliver that debt across a restart. Closing the window fully
/// needs an offer-hold primitive in lampo; this proves the happy path
/// runs.
#[tokio::test]
#[ignore = "needs the spark operator stack and bitcoind, see the module docs"]
async fn direction_b_full_swap_lightning_to_spark() {
    use lampo_common::model::{request, response};
    use lampo_testing::prelude::*;
    use lampo_testing::LampoTesting;

    init_logger();

    // node S is the swap node's lightning node; node U is the user, who
    // pays over lightning and receives on spark. U funds a channel to S
    // so S has the inbound to receive the offer payment.
    let node_s = std::sync::Arc::new(LampoTesting::tmp().await.expect("node S"));
    let node_u = LampoTesting::new(node_s.btc.clone()).await.expect("node U");
    node_u
        .fund_channel_with(node_s.clone(), 1_000_000)
        .await
        .expect("channel U -> S");

    // The swap node pays out on spark, so its wallet must be funded; the
    // user receives on spark.
    let amount_sat: u64 = 50_000;
    let amount_msat = amount_sat * 1000;
    let swapd_spark = std::sync::Arc::new(wallet(nonce()).await);
    let user_spark = wallet(nonce()).await;
    fund(&swapd_spark, amount_sat).await;
    let user_address = user_spark
        .get_spark_address()
        .unwrap()
        .to_address_string()
        .unwrap();

    let store_dir = std::env::temp_dir().join(format!("swapd-e2e-b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store_dir);
    let engine = std::sync::Arc::new(Engine::new(
        LampoLeg::new(node_s.lampod()),
        SparkLeg::new(swapd_spark.clone()),
        SwapStore::new(store_dir).expect("store"),
        engine_settings(90),
    ));
    tokio::spawn(engine.clone().run());

    // Publish the offer for the user's spark address, then the user pays
    // it and keeps the preimage its settlement reveals.
    let offer = engine
        .create_receive_offer(&user_address, amount_msat)
        .await
        .expect("receive offer");

    let pay: response::PayResult = node_u
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer,
                amount: None,
                bolt12: None,
            },
        )
        .await
        .expect("user pays the offer");
    assert_eq!(pay.state, response::PaymentState::Success);
    let preimage = pay.payment_preimage.expect("preimage from settlement");

    // The engine, seeing the payment, owes and creates the spark htlc.
    let mut done = false;
    for _ in 0..30 {
        if engine.list().iter().any(|s| {
            matches!(s.direction, lampo_spark_swapd::swap::Direction::LnToSpark)
                && s.state == State::Done
        }) {
            done = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    assert!(
        done,
        "the engine must deliver the spark htlc after the payment"
    );

    // The user claims the htlc with the preimage its lightning payment
    // revealed, and the spark funds land.
    let mut claimed = false;
    for _ in 0..15 {
        user_spark.sync().await.ok();
        if user_spark
            .claim_htlc(&spark::services::Preimage::from_hex(&preimage).expect("preimage"))
            .await
            .is_ok()
        {
            claimed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(claimed, "the user must claim the delivered spark htlc");

    let mut user_balance = 0;
    for _ in 0..15 {
        user_spark.sync().await.ok();
        user_balance = user_spark.get_balance().await.unwrap_or(0);
        if user_balance >= amount_sat {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(
        user_balance >= amount_sat,
        "the user must receive the spark funds, has {user_balance}"
    );
    println!(
        "direction B complete: user received {user_balance} sat on spark for a lightning payment"
    );
}
