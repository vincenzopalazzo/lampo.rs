//! Conversion helpers between lampo models and LND proto messages.

pub fn chain_network(network: &str) -> String {
    match network {
        "bitcoin" | "mainnet" => "mainnet".to_string(),
        "testnet" => "testnet".to_string(),
        "regtest" => "regtest".to_string(),
        "signet" => "signet".to_string(),
        other => other.to_string(),
    }
}

/// Accept Zeus/LND path params as either hex or base64 payment hashes.
pub fn normalize_r_hash(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(trimmed.to_lowercase());
    }
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let engine_url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    if let Ok(bytes) = engine
        .decode(trimmed)
        .or_else(|_| engine_url.decode(trimmed))
    {
        if bytes.len() == 32 {
            return Some(hex::encode(bytes));
        }
    }
    None
}
