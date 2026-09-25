//! BOLT 12 recurrence series tracking (v1).
//!
//! LDK implements the recurrence protocol but deliberately leaves long-lived
//! tracking to the node. This module owns that state: one series per offer
//! per node, keyed by hex offer id, persisted in the node store before the
//! first recurring pay is attempted.
//!
//! The recurrence id itself comes from node entropy on the first recurring
//! pay and never changes afterwards. Later pays and cancels reuse the stored
//! record; a second pay of the same offer continues the series instead of
//! minting a second id.
//!
//! [`RecurrenceStore`] is also updated from the event handler:
//! `PaymentSent` advances the series (writes `next_state`, bumps the
//! counter, clears the in-flight payment), `PaymentFailed` only clears the
//! in-flight payment so the next pay retries the same period.
use std::sync::Arc;

use lampo_common::error;
use lampo_common::ldk::offers::offer::{Offer, RecurrencePeriod};
use lampo_common::ldk::util::persist::KVStoreSync;

use crate::persistence::LampoPersistence;

/// Namespace holding one [`RecurrenceSeries`] per recurring offer, keyed by
/// hex offer id. v1 tracks a single series per offer: paying the same offer
/// twice continues the series.
pub const RECURRENCE_NAMESPACE: &str = "recurring_payments";

/// Layout version of a stored record, so the format can change later without
/// misreading old entries.
const RECORD_VERSION: u8 = 1;

/// Upper bound for payee-supplied recurrence state. LDK's own state is a
/// fixed 56 bytes; anything far beyond that is corruption or abuse, and must
/// never reach the u16 length prefix in [`RecurrenceSeries::encode`] (which
/// would panic instead of erroring).
const MAX_PREV_STATE_LEN: usize = 256;

/// Daily, weekly, or monthly: the only cadences the v1 RPC accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceCadence {
    Daily,
    Weekly,
    Monthly,
}

impl RecurrenceCadence {
    /// Parse the `recurrence` RPC flag. Rejects anything else so a typo can
    /// never start a series with an unintended period.
    pub fn parse(flag: &str) -> error::Result<Self> {
        match flag {
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            other => {
                error::bail!("unsupported recurrence `{other}`; use daily, weekly, or monthly")
            }
        }
    }

    /// Map onto the LDK offer period. Weekly is seven calendar days.
    pub fn to_ldk_period(self) -> RecurrencePeriod {
        match self {
            Self::Daily => RecurrencePeriod::Days(1),
            Self::Weekly => RecurrencePeriod::Days(7),
            Self::Monthly => RecurrencePeriod::Months(1),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

/// Persistent state of one recurring payment series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceSeries {
    /// Stable across all periods. Minted once from node entropy.
    pub recurrence_id: [u8; 32],
    /// Counter to use for the next `pay_for_recurrence` call.
    pub next_counter: u32,
    /// `start` to echo back. `None` until the first invoice anchors it.
    pub start: Option<u32>,
    /// `prev_state` for the next request. `None` for period 0 or when the
    /// payee omitted next-state.
    pub prev_state: Option<Vec<u8>>,
    /// Basetime expected in the next invoice. `None` until the first
    /// invoice establishes it.
    pub expected_basetime: Option<u64>,
    /// Payment id of the in-flight recurring pay, if any. A second pay
    /// while this is set is rejected so two periods can never race.
    pub pending_payment: Option<[u8; 32]>,
    /// Set by cancel. A cancelled series can never pay again.
    pub cancelled: bool,
}

impl RecurrenceSeries {
    /// Fresh series for period 0. Persisted before the first pay so a crash
    /// between minting the id and calling LDK still resumes with this id.
    pub fn new(recurrence_id: [u8; 32]) -> Self {
        Self {
            recurrence_id,
            next_counter: 0,
            start: None,
            prev_state: None,
            expected_basetime: None,
            pending_payment: None,
            cancelled: false,
        }
    }

    fn encode(&self) -> Vec<u8> {
        fn push_opt_u32(buf: &mut Vec<u8>, value: Option<u32>) {
            match value {
                Some(v) => {
                    buf.push(1);
                    buf.extend_from_slice(&v.to_be_bytes());
                }
                None => buf.push(0),
            }
        }
        fn push_opt_u64(buf: &mut Vec<u8>, value: Option<u64>) {
            match value {
                Some(v) => {
                    buf.push(1);
                    buf.extend_from_slice(&v.to_be_bytes());
                }
                None => buf.push(0),
            }
        }
        let mut buf = vec![RECORD_VERSION];
        buf.extend_from_slice(&self.recurrence_id);
        buf.extend_from_slice(&self.next_counter.to_be_bytes());
        push_opt_u32(&mut buf, self.start);
        match &self.prev_state {
            Some(state) => {
                buf.push(1);
                let len = u16::try_from(state.len()).expect("prev_state fits in u16");
                buf.extend_from_slice(&len.to_be_bytes());
                buf.extend_from_slice(state);
            }
            None => buf.push(0),
        }
        push_opt_u64(&mut buf, self.expected_basetime);
        match self.pending_payment {
            Some(id) => {
                buf.push(1);
                buf.extend_from_slice(&id);
            }
            None => buf.push(0),
        }
        buf.push(u8::from(self.cancelled));
        buf
    }

    fn decode(buf: &[u8]) -> error::Result<Self> {
        fn read_opt_u32(buf: &[u8], pos: &mut usize) -> error::Result<Option<u32>> {
            let flag = *buf.get(*pos).ok_or(error::anyhow!("record truncated"))?;
            *pos += 1;
            match flag {
                0 => Ok(None),
                1 => {
                    let end = pos
                        .checked_add(4)
                        .ok_or(error::anyhow!("record truncated"))?;
                    let bytes: [u8; 4] = buf
                        .get(*pos..end)
                        .ok_or(error::anyhow!("record truncated"))?
                        .try_into()
                        .map_err(|_| error::anyhow!("record truncated"))?;
                    *pos = end;
                    Ok(Some(u32::from_be_bytes(bytes)))
                }
                flag => error::bail!("unknown option flag {flag}"),
            }
        }
        fn read_opt_u64(buf: &[u8], pos: &mut usize) -> error::Result<Option<u64>> {
            let flag = *buf.get(*pos).ok_or(error::anyhow!("record truncated"))?;
            *pos += 1;
            match flag {
                0 => Ok(None),
                1 => {
                    let end = pos
                        .checked_add(8)
                        .ok_or(error::anyhow!("record truncated"))?;
                    let bytes: [u8; 8] = buf
                        .get(*pos..end)
                        .ok_or(error::anyhow!("record truncated"))?
                        .try_into()
                        .map_err(|_| error::anyhow!("record truncated"))?;
                    *pos = end;
                    Ok(Some(u64::from_be_bytes(bytes)))
                }
                flag => error::bail!("unknown option flag {flag}"),
            }
        }
        if buf.first() != Some(&RECORD_VERSION) {
            error::bail!("unsupported recurrence record version");
        }
        let mut pos = 1usize;
        let take = |buf: &[u8], pos: &mut usize, len: usize| -> error::Result<Vec<u8>> {
            let end = pos
                .checked_add(len)
                .ok_or(error::anyhow!("record truncated"))?;
            let out = buf
                .get(*pos..end)
                .ok_or(error::anyhow!("record truncated"))?
                .to_vec();
            *pos = end;
            Ok(out)
        };
        let recurrence_id: [u8; 32] = take(buf, &mut pos, 32)?
            .try_into()
            .map_err(|_| error::anyhow!("record truncated"))?;
        let next_counter = u32::from_be_bytes(
            take(buf, &mut pos, 4)?
                .try_into()
                .map_err(|_| error::anyhow!("record truncated"))?,
        );
        let start = read_opt_u32(buf, &mut pos)?;
        let prev_state = match *buf.get(pos).ok_or(error::anyhow!("record truncated"))? {
            0 => {
                pos += 1;
                None
            }
            1 => {
                pos += 1;
                let len = u16::from_be_bytes(
                    take(buf, &mut pos, 2)?
                        .try_into()
                        .map_err(|_| error::anyhow!("record truncated"))?,
                ) as usize;
                Some(take(buf, &mut pos, len)?)
            }
            flag => error::bail!("unknown option flag {flag}"),
        };
        let expected_basetime = read_opt_u64(buf, &mut pos)?;
        let pending_payment = match *buf.get(pos).ok_or(error::anyhow!("record truncated"))? {
            flag => {
                pos += 1;
                match flag {
                    0 => None,
                    1 => Some(
                        take(buf, &mut pos, 32)?
                            .try_into()
                            .map_err(|_| error::anyhow!("record truncated"))?,
                    ),
                    flag => error::bail!("unknown option flag {flag}"),
                }
            }
        };
        let cancelled = match *buf.get(pos).ok_or(error::anyhow!("record truncated"))? {
            0 => false,
            1 => true,
            flag => error::bail!("unknown cancelled flag {flag}"),
        };
        pos += 1;
        if pos != buf.len() {
            error::bail!("recurrence record has {} trailing bytes", buf.len() - pos);
        }
        Ok(Self {
            recurrence_id,
            next_counter,
            start,
            prev_state,
            expected_basetime,
            pending_payment,
            cancelled,
        })
    }
}

/// Storage key: hex offer id. One series per offer in v1.
pub fn series_key(offer: &Offer) -> String {
    lampo_common::hex::encode(offer.id().0)
}

/// Load/save [`RecurrenceSeries`] records. Takes the concrete store on
/// purpose, same as the payer proof store: one backend today.
#[derive(Clone)]
pub struct RecurrenceStore {
    persister: Arc<LampoPersistence>,
}

impl RecurrenceStore {
    pub fn new(persister: Arc<LampoPersistence>) -> Self {
        Self { persister }
    }

    pub fn load(&self, offer_key: &str) -> error::Result<Option<RecurrenceSeries>> {
        match self.persister.read(RECURRENCE_NAMESPACE, "", offer_key) {
            Ok(buf) => Ok(Some(RecurrenceSeries::decode(&buf)?)),
            Err(err) if err.kind() == lampo_common::ldk::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save(&self, offer_key: &str, series: &RecurrenceSeries) -> error::Result<()> {
        self.persister
            .write(RECURRENCE_NAMESPACE, "", offer_key, series.encode())?;
        Ok(())
    }

    /// Find the series with this in-flight payment id. Series are few
    /// (subscriptions per node), so a namespace scan is fine for v1.
    /// Find the series with this in-flight payment id. Series are few
    /// (subscriptions per node), so a namespace scan is fine for v1.
    /// A single corrupt entry must not break unrelated payments, so
    /// undecodable records are skipped with a warning.
    pub fn find_by_pending(
        &self,
        payment_id: &[u8; 32],
    ) -> error::Result<Option<(String, RecurrenceSeries)>> {
        let keys = self.persister.list(RECURRENCE_NAMESPACE, "")?;
        for key in keys {
            let series = match self.load(&key) {
                Ok(series) => series,
                Err(err) => {
                    log::warn!(target: "lampo::recurrence", "skipping corrupt series `{key}`: {err}");
                    continue;
                }
            };
            if let Some(series) = series {
                if series.pending_payment.as_ref() == Some(payment_id) {
                    return Ok(Some((key, series)));
                }
            }
        }
        Ok(None)
    }

    /// Advance the series that paid `payment_id`: record the invoice's
    /// next-state as the following request's `prev_state`, anchor the
    /// basetime, bump the counter, and clear the in-flight payment.
    /// Returns the hex recurrence id when a series matched.
    pub fn advance(
        &self,
        payment_id: &[u8; 32],
        next_state: Option<Vec<u8>>,
        basetime: u64,
    ) -> error::Result<Option<String>> {
        if let Some(state) = next_state.as_ref() {
            if state.len() > MAX_PREV_STATE_LEN {
                error::bail!("recurrence next-state too long: {} bytes", state.len());
            }
        }
        let Some((key, mut series)) = self.find_by_pending(payment_id)? else {
            return Ok(None);
        };
        series.prev_state = next_state;
        series.expected_basetime = Some(basetime);
        series.next_counter = series.next_counter.saturating_add(1);
        series.pending_payment = None;
        let recurrence_id = lampo_common::hex::encode(series.recurrence_id);
        self.save(&key, &series)?;
        Ok(Some(recurrence_id))
    }

    /// Clear the in-flight payment after a terminal failure. The counter and
    /// state are left untouched so the next pay retries the same period.
    pub fn clear_pending(&self, payment_id: &[u8; 32]) -> error::Result<()> {
        let Some((key, mut series)) = self.find_by_pending(payment_id)? else {
            return Ok(());
        };
        series.pending_payment = None;
        self.save(&key, &series)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_parses_rpc_flag() {
        assert_eq!(
            RecurrenceCadence::parse("daily").unwrap(),
            RecurrenceCadence::Daily
        );
        assert_eq!(
            RecurrenceCadence::parse("weekly").unwrap(),
            RecurrenceCadence::Weekly
        );
        assert_eq!(
            RecurrenceCadence::parse("monthly").unwrap(),
            RecurrenceCadence::Monthly
        );
        assert!(RecurrenceCadence::parse("yearly").is_err());
        assert!(RecurrenceCadence::parse("").is_err());
    }

    #[test]
    fn cadence_maps_to_ldk_period() {
        assert_eq!(
            RecurrenceCadence::Daily.to_ldk_period(),
            RecurrencePeriod::Days(1)
        );
        assert_eq!(
            RecurrenceCadence::Weekly.to_ldk_period(),
            RecurrencePeriod::Days(7)
        );
        assert_eq!(
            RecurrenceCadence::Monthly.to_ldk_period(),
            RecurrencePeriod::Months(1)
        );
    }

    #[test]
    fn fresh_series_starts_at_period_zero() {
        let series = RecurrenceSeries::new([9u8; 32]);
        assert_eq!(series.next_counter, 0);
        assert!(series.prev_state.is_none());
        assert!(series.pending_payment.is_none());
        assert!(!series.cancelled);
    }

    #[test]
    fn series_roundtrips_with_all_fields() {
        let series = RecurrenceSeries {
            recurrence_id: [1u8; 32],
            next_counter: 3,
            start: Some(12),
            prev_state: Some(vec![7u8; 56]),
            expected_basetime: Some(1_700_000_000),
            pending_payment: Some([2u8; 32]),
            cancelled: false,
        };
        let decoded = RecurrenceSeries::decode(&series.encode()).unwrap();
        assert_eq!(decoded, series);
    }

    #[test]
    fn series_roundtrips_empty_optionals() {
        let series = RecurrenceSeries::new([3u8; 32]);
        let decoded = RecurrenceSeries::decode(&series.encode()).unwrap();
        assert_eq!(decoded, series);
    }

    #[test]
    fn decode_rejects_truncated_record() {
        assert!(RecurrenceSeries::decode(&[RECORD_VERSION, 0, 0]).is_err());
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let mut buf = RecurrenceSeries::new([1u8; 32]).encode();
        buf[0] = RECORD_VERSION + 1;
        assert!(RecurrenceSeries::decode(&buf).is_err());
    }

    #[test]
    fn series_survives_store_reopen() {
        // Simulates a node restart: the series must reload from disk with
        // the same id, counter, and state, so the next pay continues it.
        let dir = std::env::temp_dir().join(format!("lampo-recur-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = "deadbeef".to_owned();
        let series = RecurrenceSeries {
            recurrence_id: [5u8; 32],
            next_counter: 2,
            start: None,
            prev_state: Some(vec![9u8; 56]),
            expected_basetime: Some(1_700_000_000),
            pending_payment: None,
            cancelled: false,
        };
        {
            let store = RecurrenceStore::new(Arc::new(LampoPersistence::new(dir.clone())));
            store.save(&key, &series).unwrap();
        }
        {
            let store = RecurrenceStore::new(Arc::new(LampoPersistence::new(dir.clone())));
            let loaded = store
                .load(&key)
                .unwrap()
                .expect("series must survive a store reopen");
            assert_eq!(loaded, series);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
