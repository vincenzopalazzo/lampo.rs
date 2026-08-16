use std::time::Duration;

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

/// How long a `pay` / `keysend` RPC waits for the terminal `PaymentEvent`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Apiv2Schema)]
#[serde(rename_all = "lowercase")]
pub enum PayTimeout {
    /// 30 s — quick feedback for scripts and local testing.
    Fast,
    /// 120 s — default; matches the soak harness curl timeout.
    #[default]
    Medium,
    /// 600 s — slow routing or BOLT12 offer flows.
    Large,
}

impl PayTimeout {
    pub fn duration(self) -> Duration {
        match self {
            Self::Fast => Duration::from_secs(30),
            Self::Medium => Duration::from_secs(120),
            Self::Large => Duration::from_secs(600),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_presets() {
        assert_eq!(PayTimeout::Fast.duration(), Duration::from_secs(30));
        assert_eq!(PayTimeout::Medium.duration(), Duration::from_secs(120));
        assert_eq!(PayTimeout::Large.duration(), Duration::from_secs(600));
    }

    #[test]
    fn deserializes_lowercase_names() {
        let fast: PayTimeout = serde_json::from_str("\"fast\"").unwrap();
        let medium: PayTimeout = serde_json::from_str("\"medium\"").unwrap();
        let large: PayTimeout = serde_json::from_str("\"large\"").unwrap();
        assert_eq!(fast, PayTimeout::Fast);
        assert_eq!(medium, PayTimeout::Medium);
        assert_eq!(large, PayTimeout::Large);
    }
}
