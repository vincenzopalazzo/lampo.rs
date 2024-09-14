use std::collections::HashMap;
use std::ops::Not;
use std::sync::Arc;

use bdk::bitcoin::Amount;
use bdk::keys::bip39::Language;
use bdk::keys::bip39::Mnemonic;
use bdk::keys::bip39::WordCount;
use bdk::keys::DerivableKey;
use bdk::keys::ExtendedKey;
use bdk::keys::GeneratableKey;
use bdk::keys::GeneratedKey;
use bdk::template::Bip84;
use bdk::KeychainKind;
use bitcoin_hashes::hex::HexIterator;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use lampo_common::utils::shutter::Shutter;
use lampo_vls::VLSKeys;

use lampo_common::bitcoin;
use lampo_common::bitcoin::consensus::Decodable;
use lampo_common::conf::{LampoConf, Network};
use lampo_common::error;
use lampo_common::json;
use lampo_common::json::Deserialize;
use lampo_common::keys::LampoKeys;
use lampo_common::model::response::{NewAddress, Utxo};
use lampo_common::wallet::WalletManager;

pub struct CoreWalletManager {
    rpc: Client,
    keymanager: Arc<LampoKeys>,
    network: Network,
}

impl CoreWalletManager {
    /// Build from mnemonic_words and return bkd::Wallet or bdk::Error
    fn build_wallet(
        conf: Arc<LampoConf>,
        mnemonic_words: &str,
        shutter: Option<Arc<Shutter>>
    ) -> error::Result<(bdk::Wallet, LampoKeys)> {
        // Parse a mnemonic
        let mnemonic = Mnemonic::parse(mnemonic_words).map_err(|err| error::anyhow!("{err}"))?;
        // Generate the extended key
        let xkey: ExtendedKey = mnemonic.into_extended_key()?;
        let network = match conf.network.to_string().as_str() {
            "bitcoin" => bdk::bitcoin::Network::Bitcoin,
            "testnet" => bdk::bitcoin::Network::Testnet,
            "signet" => bdk::bitcoin::Network::Signet,
            "regtest" => bdk::bitcoin::Network::Regtest,
            _ => unreachable!(),
        };
        // Get xprv from the extended key
        let xprv = xkey
            .into_xprv(network)
            .ok_or(error::anyhow!("impossible cast the private key"))?;

        let vls_grpc = VLSKeys.create_keys_manager(conf.clone(), &xprv.private_key.secret_bytes(), conf.vls_port, shutter);
        let keys_manger = vls_grpc.keys_manager;
        let ldk_keys = LampoKeys::new(xprv.private_key.secret_bytes(), conf, keys_manger);
        // Create a BDK wallet structure using BIP 84 descriptor ("m/84h/1h/0h/0" and "m/84h/1h/0h/1")
        let wallet = bdk::Wallet::new(
            Bip84(xprv, KeychainKind::External),
            Some(Bip84(xprv, KeychainKind::Internal)),
            (),
            network,
        )?;
        Ok((wallet, ldk_keys))
    }

    #[cfg(debug_assertions)]
    fn build_from_private_key(
        xprv: lampo_common::bitcoin::PrivateKey,
        channel_keys: Option<String>,
        conf: Arc<LampoConf>,
        shutter: Option<Arc<Shutter>>
    ) -> error::Result<(bdk::Wallet, LampoKeys)> {
        use bdk::bitcoin::bip32::Xpriv;

        let vls_grpc = VLSKeys.create_keys_manager(conf.clone(), &xprv.inner.secret_bytes(), conf.vls_port, shutter);
        let keys_manger = vls_grpc.keys_manager;

        let ldk_keys = if let Some(channel_keys) = channel_keys {
            LampoKeys::with_channel_keys(xprv.inner.secret_bytes(), channel_keys, conf, keys_manger)
        } else {
            LampoKeys::new(xprv.inner.secret_bytes(), conf, keys_manger)
        };
        let network = match xprv.network.to_string().as_str() {
            "bitcoin" => bdk::bitcoin::Network::Bitcoin,
            "testnet" => bdk::bitcoin::Network::Testnet,
            "signet" => bdk::bitcoin::Network::Signet,
            "regtest" => bdk::bitcoin::Network::Regtest,
            _ => unreachable!(),
        };
        let key = Xpriv::new_master(network, &xprv.inner.secret_bytes())?;
        let key = ExtendedKey::from(key);
        let wallet = bdk::Wallet::new(Bip84(key, KeychainKind::External), None, (), network)
            .map_err(|err| error::anyhow!(err.to_string()))?;
        Ok((wallet, ldk_keys))
    }

    fn configure_bitcoin_wallet(
        rpc: &Client,
        conf: Arc<LampoConf>,
        wallet: bdk::Wallet,
    ) -> error::Result<String> {
        // FIXME: allow to support multiple wallets for the same chain, so
        // we should make a suffix in the following name
        let name_wallet = "lampo-wallet".to_owned();
        if !rpc
            .list_wallets()?
            .iter()
            .any(|wallet| wallet == &name_wallet)
        {
            let result: Result<json::Value, bitcoincore_rpc::Error> = rpc.call(
                "createwallet",
                &[
                    name_wallet.clone().into(),
                    false.into(),
                    false.into(),
                    json::Value::Null,
                    false.into(),
                    true.into(),
                    true.into(),
                    false.into(),
                ],
            );
            if result.is_err() {
                let _ = rpc.load_wallet(&name_wallet)?;
            } else {
                let external_signer = wallet.get_signers(KeychainKind::External);
                let external_signer = external_signer.as_key_map(wallet.secp_ctx());
                let external_descriptor =
                    wallet.get_descriptor_for_keychain(KeychainKind::External);
                let external_descriptor =
                    external_descriptor.to_string_with_secret(&external_signer);
                let internal_signer = wallet.get_signers(KeychainKind::Internal);
                let internal_signer = internal_signer.as_key_map(wallet.secp_ctx());
                let internal_descriptor =
                    wallet.get_descriptor_for_keychain(KeychainKind::Internal);
                let internal_descriptor =
                    internal_descriptor.to_string_with_secret(&internal_signer);

                let options = vec![
                    json::json!({
                        "desc": external_descriptor,
                        "active": true,
                        "timestamp": "now",
                        "internal": false,
                    }),
                    json::json!({
                        "desc": internal_descriptor,
                        "active": true,
                        "timestamp": "now",
                        "internal": true,
                    }),
                ];

                let rpc = Self::build_bitcoin_rpc(conf.clone(), Some(&name_wallet))?;
                log::trace!(target: "core", "import descriptor options: {:?}", options);
                let _: json::Value = rpc.call("importdescriptors", &[json::json!(options)])?;
            }
        };

        Ok(name_wallet)
    }

    fn build_bitcoin_rpc(conf: Arc<LampoConf>, wallet: Option<&str>) -> error::Result<Client> {
        let mut url = conf
            .core_url
            .clone()
            .ok_or(error::anyhow!("bitcoin core url not specified"))?
            .as_str()
            .to_owned();
        if let Some(wallet_name) = wallet {
            url = format!("{url}/wallet/{wallet_name}");
        }
        let rpc = Client::new(
            &url,
            Auth::UserPass(
                conf.core_user
                    .clone()
                    .ok_or(error::anyhow!("bitcoin core user not specified"))?,
                conf.core_pass
                    .clone()
                    .ok_or(error::anyhow!("bitcoin core password not specified"))?,
            ),
        )?;
        Ok(rpc)
    }
}

#[macro_export]
macro_rules! hex (($hex:expr) => (<Vec<u8> as bitcoin_hashes::hex::FromHex>::from_hex($hex).unwrap()));

#[macro_export]
macro_rules! serialize {
    ( $x:expr ) => {
        Ok(bitcoin::consensus::deserialize(&hex!(&$x))?)
    };
}

#[derive(Debug, Deserialize)]
struct Tx {
    hex: Option<String>,
}

impl WalletManager for CoreWalletManager {
    fn new(conf: Arc<LampoConf>, shutter: Option<Arc<Shutter>>) -> error::Result<(Self, String)>
    where
        Self: Sized,
    {
        let mnemonic: GeneratedKey<_, bdk::miniscript::Tap> =
            Mnemonic::generate((WordCount::Words12, Language::English))
                .map_err(|err| error::anyhow!("{:?}", err))?;

        let (wallet, keymanager) =
            CoreWalletManager::build_wallet(conf.clone(), &mnemonic.to_string(), shutter)?;
        let rpc = Self::build_bitcoin_rpc(conf.clone(), None)?;
        let wallet_name = Self::configure_bitcoin_wallet(&rpc, conf.clone(), wallet)?;
        let rpc = Self::build_bitcoin_rpc(conf.clone(), Some(&wallet_name))?;
        Ok((
            Self {
                rpc,
                keymanager: keymanager.into(),
                network: conf.network,
            },
            mnemonic.to_string(),
        ))
    }

    fn create_transaction(
        &self,
        script: bitcoin::ScriptBuf,
        amount_sat: u64,
        fee_rate: u32,
    ) -> error::Result<bitcoin::Transaction> {
        let addr = bitcoin_bech32::WitnessProgram::from_scriptpubkey(
            script.as_bytes(),
            match self.network {
                Network::Bitcoin => bitcoin_bech32::constants::Network::Bitcoin,
                Network::Testnet => bitcoin_bech32::constants::Network::Testnet,
                Network::Regtest => bitcoin_bech32::constants::Network::Regtest,
                Network::Signet => bitcoin_bech32::constants::Network::Signet,
                _ => error::bail!("network `{}` not supported", self.network),
            },
        )?
        .to_address();
        let mut map = HashMap::new();
        map.insert(addr, Amount::from_sat(amount_sat).to_btc());
        let options = json::json!({
            // LDK gives us feerates in satoshis per KW but Bitcoin Core here expects fees
            // denominated in satoshis per vB. First we need to multiply by 4 to convert weight
            // units to virtual bytes, then divide by 1000 to convert KvB to vB.
            "fee_rate": fee_rate as f64 / 250.0,
            // While users could "cancel" a channel open by RBF-bumping and paying back to
            // themselves, we don't allow it here as its easy to have users accidentally RBF bump
            // and pay to the channel funding address, which results in loss of funds. Real
            // LDK-based applications should enable RBF bumping and RBF bump either to a local
            // change address or to a new channel output negotiated with the same node.
            "replaceable": false,
            "include_unsafe": true,
            "includeWatching": true,
            "add_inputs": true,
        });

        let hex: String = self.rpc.call(
            "createrawtransaction",
            &[json::json!([]), json::json!(&map), json::json!(0)],
        )?;

        let tx: Tx = self.rpc.call(
            "fundrawtransaction",
            &[json::json!(hex), json::json!(options)],
        )?;

        let hex: Tx = self
            .rpc
            .call("signrawtransactionwithwallet", &[json::json!(tx.hex)])?;
        let hex = hex.hex.unwrap();
        let mut reader = HexIterator::new(&hex)?;
        let object = Decodable::consensus_decode(&mut reader)?;
        Ok(object)
    }

    fn get_onchain_address(&self) -> error::Result<NewAddress> {
        let addr = self.rpc.call("getnewaddress", &["lampo-addr".into()])?;
        log::debug!(target: "core-wallet", "addr generated: {addr}" );
        Ok(NewAddress { address: addr })
    }

    fn get_onchain_balance(&self) -> error::Result<u64> {
        let balance = self.rpc.get_balance(None, Some(true))?;
        Ok(balance.to_sat() * 1000)
    }

    fn ldk_keys(&self) -> Arc<LampoKeys> {
        self.keymanager.clone()
    }

    fn list_transactions(&self) -> error::Result<Vec<Utxo>> {
        let unspend = self
            .rpc
            .list_unspent(None, None, None, Some(true), None)?
            .iter()
            .map(|utxo| Utxo {
                txid: utxo.txid.to_string(),
                vout: utxo.vout,
                reserved: utxo.spendable.not(),
                confirmed: utxo.confirmations,
                amount_msat: utxo.amount.to_sat() * 1000,
            })
            .collect::<Vec<_>>();
        Ok(unspend)
    }

    fn restore(conf: Arc<LampoConf>, mnemonic_words: &str, shutter: Option<Arc<Shutter>>) -> error::Result<Self>
    where
        Self: Sized,
    {
        let (wallet, keymanager) = CoreWalletManager::build_wallet(conf.clone(), mnemonic_words, shutter)?;

        let rpc = Client::new(
            conf.core_url
                .clone()
                .ok_or(error::anyhow!("bitcoin core url not specified"))?
                .as_str(),
            Auth::UserPass(
                conf.core_user
                    .clone()
                    .ok_or(error::anyhow!("bitcoin core user not specified"))?,
                conf.core_pass
                    .clone()
                    .ok_or(error::anyhow!("bitcoin core password not specified"))?,
            ),
        )?;

        Self::configure_bitcoin_wallet(&rpc, conf.clone(), wallet)?;
        Ok(Self {
            rpc,
            keymanager: keymanager.into(),
            network: conf.network,
        })
    }

    fn sync(&self) -> error::Result<()> {
        Ok(())
    }
}
