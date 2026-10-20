//! `listpayments`: the node's payment history, out of the store.
use paperclip::actix::Apiv2Schema;

use crate::json::{Deserialize, Serialize};
use crate::persist::{PaymentFilter, PaymentRecord};

pub mod request {
    use super::*;

    /// Which payments to return. Everything unset means all of them, oldest
    /// first.
    #[derive(Debug, Clone, Default, Serialize, Deserialize, Apiv2Schema)]
    pub struct ListPayments {
        /// Only payments created at or after this Unix timestamp.
        pub from: Option<u64>,
        /// Only payments created at or before this Unix timestamp.
        pub to: Option<u64>,
        /// `inbound` or `outbound`.
        pub direction: Option<String>,
        /// `pending`, `succeeded` or `failed`.
        pub status: Option<String>,
        pub limit: Option<u64>,
        pub offset: Option<u64>,
    }

    impl ListPayments {
        /// Translate into the store's filter, rejecting unknown values rather
        /// than silently returning everything.
        pub fn to_filter(&self) -> crate::error::Result<PaymentFilter> {
            use crate::persist::{PaymentDirection, PaymentStatus};
            Ok(PaymentFilter {
                from_unix_secs: self.from,
                to_unix_secs: self.to,
                direction: self
                    .direction
                    .as_deref()
                    .map(PaymentDirection::from_str)
                    .transpose()?,
                status: self
                    .status
                    .as_deref()
                    .map(PaymentStatus::from_str)
                    .transpose()?,
                limit: self.limit,
                offset: self.offset,
            })
        }
    }
}

pub mod response {
    use super::*;

    /// One payment as reported over RPC.
    #[derive(Debug, Clone, Serialize, Deserialize, Apiv2Schema)]
    pub struct Payment {
        pub id: String,
        pub payment_hash: String,
        pub direction: String,
        pub amount_msat: u64,
        pub fee_msat: Option<u64>,
        pub status: String,
        pub created_at: u64,
        pub invoice: Option<String>,
    }

    impl From<PaymentRecord> for Payment {
        fn from(record: PaymentRecord) -> Self {
            Self {
                id: record.id,
                payment_hash: record.payment_hash,
                direction: record.direction.as_str().to_owned(),
                amount_msat: record.amount_msat,
                fee_msat: record.fee_msat,
                status: record.status.as_str().to_owned(),
                created_at: record.created_at,
                invoice: record.invoice,
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Apiv2Schema)]
    pub struct ListPayments {
        pub payments: Vec<Payment>,
    }
}
