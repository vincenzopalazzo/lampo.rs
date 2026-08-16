#[cfg(test)]
mod wire_tests {
    use crate::convert;
    use crate::lnrpc;
    use serde::Deserialize;

    #[test]
    fn getinfo_uint64_serializes_as_string() {
        let info = lnrpc::GetInfoResponse {
            version: "0.18.5-beta".into(),
            identity_pubkey: "02ab".into(),
            block_height: 42,
            synced_to_chain: true,
            chains: vec![lnrpc::Chain {
                network: "regtest".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["version"], "0.18.5-beta");
        assert_eq!(value["identityPubkey"], "02ab");
        assert_eq!(value["blockHeight"], 42);
        assert_eq!(value["chains"][0]["network"], "regtest");
    }

    #[test]
    fn wallet_balance_int64_fields_present() {
        let bal = lnrpc::WalletBalanceResponse {
            total_balance: 1500,
            confirmed_balance: 1000,
            unconfirmed_balance: 500,
            ..Default::default()
        };
        let value = serde_json::to_value(&bal).unwrap();
        assert_eq!(value["confirmedBalance"], "1000");
        assert_eq!(value["totalBalance"], "1500");
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SendPaymentBody {
        #[serde(default, alias = "payment_request")]
        payment_request: String,
    }

    #[test]
    fn accepts_zeus_camel_case_payment_request() {
        let body: SendPaymentBody =
            serde_json::from_str(r#"{"paymentRequest":"lnbc1..."}"#).unwrap();
        assert_eq!(body.payment_request, "lnbc1...");
    }

    #[test]
    fn normalize_r_hash_accepts_hex_and_base64() {
        let hex = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(convert::normalize_r_hash(hex).as_deref(), Some(hex));
        use base64::Engine;
        let bytes = vec![0xaa; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        assert_eq!(convert::normalize_r_hash(&b64).as_deref(), Some(hex));
    }
}
