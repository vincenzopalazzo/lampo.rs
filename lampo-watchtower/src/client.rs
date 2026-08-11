//! HTTP client for the tower's public API.

use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
use bitcoin::Txid;

use crate::outbox::SignedJustice;
use crate::teos;

/// Errors from a tower interaction, split by how the delivery loop
/// should react.
#[derive(Debug)]
pub enum TowerError {
    /// The tower is unreachable or misbehaving at the transport level:
    /// retry later with backoff.
    Connection(String),
    /// The tower replied with a protocol error.
    Api(teos::ApiError),
    /// The tower replied with a receipt not signed by the expected
    /// tower id: proof of misbehavior, do not trust it.
    Signature(String),
}

impl std::fmt::Display for TowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TowerError::Connection(err) => write!(f, "connection error: {err}"),
            TowerError::Api(err) => {
                write!(f, "tower error (code {}): {}", err.error_code, err.error)
            }
            TowerError::Signature(err) => write!(f, "tower signature mismatch: {err}"),
        }
    }
}

/// A client bound to one tower and one user key.
pub struct TowerClient {
    base_url: String,
    tower_id: PublicKey,
    user_sk: SecretKey,
    user_id: PublicKey,
    http: reqwest::Client,
}

impl TowerClient {
    pub fn new(base_url: String, tower_id: PublicKey, user_sk: SecretKey) -> Self {
        let user_id = PublicKey::from_secret_key(&Secp256k1::new(), &user_sk);
        TowerClient {
            base_url: base_url.trim_end_matches('/').to_owned(),
            tower_id,
            user_sk,
            user_id,
            http: reqwest::Client::new(),
        }
    }

    pub fn user_id(&self) -> PublicKey {
        self.user_id
    }

    async fn post<Request: serde::Serialize, Response: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &Request,
    ) -> Result<Response, TowerError> {
        let url = format!("{}/{endpoint}", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|err| TowerError::Connection(format!("{err}")))?;

        if response.status().is_success() {
            response
                .json::<Response>()
                .await
                .map_err(|err| TowerError::Connection(format!("bad response body: {err}")))
        } else {
            let err = response
                .json::<teos::ApiError>()
                .await
                .map_err(|err| TowerError::Connection(format!("bad error body: {err}")))?;
            Err(TowerError::Api(err))
        }
    }

    /// Registers (or tops up the subscription of) the user with the
    /// tower, verifying the subscription receipt.
    pub async fn register(&self) -> Result<teos::RegisterResponse, TowerError> {
        let request = teos::RegisterRequest {
            user_id: self.user_id.serialize().to_vec(),
        };
        let response: teos::RegisterResponse = self.post("register", &request).await?;
        if !response.verify(&self.tower_id) {
            return Err(TowerError::Signature(
                "registration receipt not signed by the configured tower id".to_owned(),
            ));
        }
        Ok(response)
    }

    /// Sends one appointment to the tower, verifying the appointment
    /// receipt signature.
    pub async fn add_appointment(
        &self,
        signed: &SignedJustice,
    ) -> Result<teos::AddAppointmentResponse, TowerError> {
        let penalty_tx = signed
            .tx()
            .map_err(|err| TowerError::Connection(format!("corrupted outbox entry: {err}")))?;
        let locator = teos::Locator::new(signed.dispute_txid);
        let encrypted_blob = teos::encrypt(&penalty_tx, &signed.dispute_txid)
            .map_err(|err| TowerError::Connection(format!("{err}")))?;
        let appointment = teos::Appointment::new(locator, encrypted_blob, signed.to_self_delay);
        let user_signature = teos::sign(&appointment.to_vec(), &self.user_sk);

        let request = teos::AddAppointmentRequest {
            appointment: teos::WireAppointment {
                locator: appointment.locator.to_vec(),
                encrypted_blob: appointment.encrypted_blob.clone(),
                to_self_delay: appointment.to_self_delay,
            },
            signature: user_signature.clone(),
        };
        let response: teos::AddAppointmentResponse = self.post("add_appointment", &request).await?;
        if !response.verify(&user_signature, &self.tower_id) {
            return Err(TowerError::Signature(format!(
                "appointment receipt for {locator} not signed by the configured tower id"
            )));
        }
        Ok(response)
    }

    /// Queries the tower for the state of an appointment. Returns the
    /// raw JSON body; the interesting field is `status`
    /// (`being_watched` or `dispute_responded`).
    pub async fn get_appointment(
        &self,
        dispute_txid: &Txid,
    ) -> Result<serde_json::Value, TowerError> {
        let locator = teos::Locator::new(*dispute_txid);
        let signature = teos::sign(
            format!("get appointment {locator}").as_bytes(),
            &self.user_sk,
        );
        let request = teos::GetAppointmentRequest {
            locator: locator.to_vec(),
            signature,
        };
        self.post("get_appointment", &request).await
    }
}
