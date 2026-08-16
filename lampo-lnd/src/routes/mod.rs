mod handlers;

pub use handlers::configure;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::HttpRequest;
use tokio::sync::RwLock;

use crate::auth::{AuthError, MacaroonBakery, Permission};
use crate::lnrpc;
use lampod::LampoDaemon;

/// In-memory invoice index so Zeus can poll `GET /v1/invoice/{r_hash}`.
#[derive(Default)]
pub struct InvoiceIndex {
    by_hash_hex: HashMap<String, lnrpc::Invoice>,
}

impl InvoiceIndex {
    pub fn insert(&mut self, r_hash_hex: String, invoice: lnrpc::Invoice) {
        self.by_hash_hex.insert(r_hash_hex, invoice);
    }

    pub fn get(&self, r_hash_hex: &str) -> Option<lnrpc::Invoice> {
        self.by_hash_hex.get(r_hash_hex).cloned()
    }

    pub fn list(&self) -> Vec<lnrpc::Invoice> {
        self.by_hash_hex.values().cloned().collect()
    }

    pub fn mark_settled(
        &mut self,
        r_hash_hex: &str,
        preimage: bytes::Bytes,
        amt_paid_msat: i64,
    ) -> bool {
        let Some(inv) = self.by_hash_hex.get_mut(r_hash_hex) else {
            return false;
        };
        inv.state = lnrpc::invoice::InvoiceState::Settled as i32;
        inv.settled = true;
        inv.r_preimage = preimage;
        inv.amt_paid_msat = amt_paid_msat;
        inv.amt_paid_sat = amt_paid_msat / 1000;
        inv.amt_paid = amt_paid_msat / 1000;
        true
    }
}

pub struct AppState {
    pub lampod: Arc<LampoDaemon>,
    pub bakery: Arc<MacaroonBakery>,
    pub invoices: Arc<RwLock<InvoiceIndex>>,
}

pub fn extract_macaroon_hex(req: &HttpRequest) -> Result<String, AuthError> {
    // Zeus / LND REST use Grpc-Metadata-macaroon (case-insensitive).
    for (name, value) in req.headers().iter() {
        let key = name.as_str();
        if key.eq_ignore_ascii_case("grpc-metadata-macaroon")
            || key.eq_ignore_ascii_case("macaroon")
        {
            return value
                .to_str()
                .map(|s| s.to_string())
                .map_err(|_| AuthError::Malformed);
        }
    }
    Err(AuthError::Missing)
}

pub fn authorize(
    req: &HttpRequest,
    bakery: &MacaroonBakery,
    required: Permission,
) -> Result<(), AuthError> {
    let hex = extract_macaroon_hex(req)?;
    bakery.verify_hex(&hex, required)
}
