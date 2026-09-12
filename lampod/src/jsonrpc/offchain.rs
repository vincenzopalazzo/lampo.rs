//! Offchain RPC methods
use std::str::FromStr;
use std::time::Duration;

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::currency::LampoCurrencyConversion;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::ldk;
use lampo_common::ldk::offers::contacts::{ContactSecret, ContactSecrets};
use lampo_common::ldk::offers::nonce::Nonce;
use lampo_common::ldk::offers::offer;
use lampo_common::ldk::offers::offer::Amount;
use lampo_common::ldk::offers::offer::CurrencyCode;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::util::ser::Writeable;
use lampo_common::model::request::AddContact;
use lampo_common::model::request::GenerateAsyncInvoicePaths;
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::request::GenerateOffer;
use lampo_common::model::request::KeySend;
use lampo_common::model::request::ListContacts;
use lampo_common::model::request::Pay;
use lampo_common::model::request::SetAsyncInvoicePaths;
use lampo_common::model::response::ContactInfo;
use lampo_common::model::response::Contacts;
use lampo_common::model::response::PayResult;
use lampo_common::model::response::{self, Decode};
use lampo_common::model::response::{Bolt11InvoiceInfo, Bolt12InvoiceInfo, Invoice};
use lampo_common::types::NodeId;
use lampo_common::{json, model::request::DecodeInvoice};
use tokio::time::Instant;

use crate::ln::{Contact, ContactPaymentParams};
use crate::LampoDaemon;

pub async fn json_invoice(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `invoice` with request `{:?}`", request);
    let request: GenerateInvoice = json::from_value(request.clone())?;
    let invoice = ctx.offchain_manager().generate_invoice(
        request.amount_msat,
        &request.description,
        request.expiring_in.unwrap_or(10000),
    )?;
    let invoice = Invoice {
        bolt11: invoice.to_string(),
    };
    Ok(json::to_value(&invoice)?)
}

pub async fn json_offer(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `offer` with request `{:?}`", request);
    let request: GenerateOffer = json::from_value(request.clone())?;

    // An async recipient's offer is built interactively with its static
    // invoice server; description/amount cannot be applied to it.
    if request.currency.is_some() || request.currency_amount.is_some() {
        if request.amount_msat.is_some() {
            return Err(crate::rpc_error!(
                "amount_msat and currency are mutually exclusive"
            ));
        }
        if request.currency.is_none() || request.currency_amount.is_none() {
            return Err(crate::rpc_error!(
                "currency and currency_amount must be set together"
            ));
        }
    }

    if ctx.offchain_manager().async_receive_enabled() {
        if request.description.is_some()
            || request.amount_msat.is_some()
            || request.currency.is_some()
        {
            return Err(crate::rpc_error!(
                "description/amount_msat/currency cannot be set on an async receive offer; the offer is built with the static invoice server"
            ));
        }
        let offer: response::Offer = ctx
            .offchain_manager()
            .async_offer()
            .map_err(|err| crate::rpc_error!("{err:?}"))?
            .into();
        return Ok(json::to_value(&offer)?);
    }

    let manager = ctx.channel_manager().manager();
    let conversion = LampoCurrencyConversion::from_conf(&ctx.conf())
        .map_err(|err| crate::rpc_error!("{err}"))?;

    // Currency offers need a builder that borrowed the converter. Bitcoin
    // offers use the same builder with a rate table that may be empty:
    // Null-equivalent until amount() is called with Amount::Currency.
    let mut offer_builder = manager
        .create_offer_builder_with_conversion(&conversion)
        .map_err(|err| crate::rpc_error!("{:?}", err))?;

    if let Some(description) = request.description {
        offer_builder = offer_builder.description(description);
    }

    if let Some(amount_msat) = request.amount_msat {
        offer_builder = offer_builder.amount_msats(amount_msat);
    }

    if let (Some(currency), Some(minor_units)) = (request.currency, request.currency_amount) {
        let code = currency.to_ascii_uppercase();
        let iso4217_code = CurrencyCode::from_str(&code)
            .map_err(|_| crate::rpc_error!("invalid currency code `{code}`"))?;
        // Fail before build if the operator has no rate for this code. LDK
        // would also reject it, but this error names the missing rate.
        let converted = conversion
            .convert_minor_units(&code, minor_units)
            .map_err(|err| crate::rpc_error!("{err}"))?;
        log::info!(
            target: "lampod::jsonrpc::offchain",
            "offer currency {code} minor_units={minor_units} -> {} msat (accepted {}..={})",
            converted.amount_msats,
            converted.minimum_msats,
            converted.maximum_msats
        );
        offer_builder = offer_builder.amount(Amount::Currency {
            iso4217_code,
            amount: minor_units,
        });
    }

    let offer: response::Offer = offer_builder
        .build()
        // FIXME: implement display error on top of the bolt12 error
        .map_err(|err| crate::rpc_error!("{:?}", err))?
        .into();
    log::debug!("Generated offer: {:?}", offer);
    Ok(json::to_value(&offer)?)
}

/// Mint hex-encoded blinded paths that an often-offline recipient installs
/// as `async-invoice-server-paths` (or via `setasyncinvoicepaths`).
///
/// Server role only. `node_id` must be a channel counterparty; those
/// pubkey bytes key the static-invoice store for this recipient.
pub async fn json_asyncinvoicepaths(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!(target: "lampod::jsonrpc::offchain", "call for `asyncinvoicepaths`");
    let request: GenerateAsyncInvoicePaths = json::from_value(request.clone())?;
    require_path_rpc_token(ctx, request.token.as_deref())?;
    let node_id = NodeId::from_str(&request.node_id)
        .map_err(|err| crate::rpc_error!("node_id is not a public key: {err}"))?;
    if ctx
        .channel_manager()
        .manager()
        .list_channels_with_counterparty(&node_id)
        .is_empty()
    {
        return Err(crate::rpc_error!("node_id must be a channel counterparty"));
    }
    let paths = ctx
        .blinded_paths_for_async_recipient(node_id.serialize().to_vec())
        .map_err(|err| crate::rpc_error!("{err}"))?;
    let paths = hex::encode(paths.encode());
    Ok(json::to_value(&response::AsyncInvoicePaths { paths })?)
}

/// Install hex-encoded blinded paths on an often-offline recipient.
/// Runtime equivalent of the `async-invoice-server-paths` config key.
pub async fn json_setasyncinvoicepaths(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!(target: "lampod::jsonrpc::offchain", "call for `setasyncinvoicepaths`");
    let request: SetAsyncInvoicePaths = json::from_value(request.clone())?;
    require_path_rpc_token(ctx, request.token.as_deref())?;
    if ctx.conf().async_payments_role.as_deref() == Some("server") && !request.force {
        return Err(crate::rpc_error!(
            "setasyncinvoicepaths is for often-offline recipients, not async-payments-role=server"
        ));
    }
    if ctx.offchain_manager().async_receive_enabled() && !request.force {
        return Err(crate::rpc_error!(
            "async receive paths already set; pass force=true to overwrite"
        ));
    }
    ctx.offchain_manager()
        .set_async_receive_paths_hex(&request.paths)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    Ok(json::to_value(&response::AsyncInvoicePaths {
        paths: request.paths,
    })?)
}

/// When `api-token` is set, the two path RPCs require a matching token.
/// Missing and wrong tokens share one error so the token is not enumerable.
fn require_path_rpc_token(ctx: &LampoDaemon, token: Option<&str>) -> Result<(), Error> {
    let conf = ctx.conf();
    let Some(expected) = conf.api_token.as_deref() else {
        return Ok(());
    };
    if token == Some(expected) {
        return Ok(());
    }
    Err(crate::rpc_error!("api-token required"))
}

pub async fn json_decode(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `invoice` with request `{:?}`", request);
    let request: DecodeInvoice = json::from_value(request.clone())?;
    if let Ok(invoice) = ctx
        .offchain_manager()
        .decode::<ldk::invoice::Bolt11Invoice>(&request.invoice_str)
    {
        let bolt11_invoice = Bolt11InvoiceInfo {
            issuer_id: invoice.payee_pub_key().map(|id| id.to_string()),
            payment_hash: hex::encode(invoice.payment_hash().0),
            timestamp: invoice.duration_since_epoch().as_secs(),
            amount_msat: invoice.amount_milli_satoshis(),
            network: invoice.network().to_string(),
            description: match invoice.description() {
                ldk::invoice::Bolt11InvoiceDescriptionRef::Direct(dec) => Some(dec.to_string()),
                ldk::invoice::Bolt11InvoiceDescriptionRef::Hash(_) => {
                    Some("description hash provided".to_string())
                }
            },
            routes: Vec::new(),
            hints: Vec::new(),
            expiry_time: Some(invoice.expiry_time().as_secs()),
        };

        return Ok(json::to_value(&Decode::from(bolt11_invoice))?);
    }

    if let Ok(offer) = ctx
        .offchain_manager()
        .decode::<ldk::offers::offer::Offer>(&request.invoice_str)
    {
        let bolt12_invoice: Bolt12InvoiceInfo = offer.into();
        return Ok(json::to_value(&Decode::from(bolt12_invoice))?);
    } else {
        Err(crate::rpc_error!("Not able to decode invoice"))
    }
}

pub async fn json_pay(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `pay` with request `{:?}`", request);
    let request: Pay = json::from_value(request.clone())?;
    let mut events = ctx.handler().events();

    let parsed_offer = offer::Offer::from_str(&request.invoice_str);
    let expected_value_msat = match &parsed_offer {
        Ok(offer) => match offer.amount() {
            Some(offer::Amount::Bitcoin { amount_msats }) => Some(amount_msats),
            _ => request.amount,
        },
        Err(_) => ctx
            .offchain_manager()
            .decode_invoice(&request.invoice_str)
            .ok()
            .and_then(|invoice| invoice.amount_milli_satoshis())
            .or(request.amount),
    };

    let payment_id = if parsed_offer.is_ok() {
        log::debug!("Paying offer with bolt12 invoice: {}", request.invoice_str);
        let bolt12 = request.bolt12;
        let payer_note = bolt12.as_ref().and_then(|x| x.payer_note.clone());
        let reveal = bolt12
            .as_ref()
            .and_then(|x| x.reveal_contact)
            .unwrap_or(false);
        let contact_params = if reveal {
            Some(build_reveal_contact_params(
                ctx,
                &request.invoice_str,
                bolt12.as_ref().and_then(|x| x.contact_label.clone()),
                bolt12.as_ref().and_then(|x| x.intro_node.clone()),
            )?)
        } else if let Some(label) = bolt12.as_ref().and_then(|x| x.contact_label.clone()) {
            // Pay back a stored contact when paying their remote offer.
            // `intro_node` is only needed if we still have to mint our compact offer.
            maybe_payback_contact_params(
                ctx,
                &request.invoice_str,
                &label,
                bolt12.as_ref().and_then(|x| x.intro_node.clone()),
            )?
        } else {
            None
        };
        ctx.offchain_manager().pay_offer_with_contact(
            &request.invoice_str,
            request.amount,
            payer_note,
            request.max_fee_msat,
            contact_params,
        )?
    } else {
        log::debug!(
            "Paying invoice with bolt11 invoice: {}",
            request.invoice_str
        );
        ctx.offchain_manager().pay_invoice(
            &request.invoice_str,
            request.amount,
            request.max_fee_msat,
            request.timeout_secs,
        )?
    };
    // The event bus broadcasts to every subscriber, so a concurrent `pay` would
    // otherwise see this payment's result -- and now its preimage and payer
    // proof too. Only accept events carrying our own payment id.
    let payment_id = hex::encode(payment_id.0);
    // LND-compatible callers use `timeout_secs` as the retry deadline before
    // an HTLC is launched. Once the payment API accepts an initial route, wait
    // for the real terminal event so an in-flight payment is never reported as
    // failed merely because that deadline elapsed.
    let timeout = terminal_wait_timeout(request.timeout_secs, request.timeout.duration());
    wait_for_payment_result(events, &payment_id, expected_value_msat, timeout).await
}

fn terminal_wait_timeout(
    compatible_retry_timeout_secs: Option<u64>,
    default_timeout: Duration,
) -> Option<Duration> {
    compatible_retry_timeout_secs
        .is_none()
        .then_some(default_timeout)
}

/// Hold the `PaymentReceipt` (preimage, payer proof) until the terminal
/// `PaymentEvent` for `payment_id` arrives, and build the `PayResult`.
/// The event bus broadcasts to every subscriber, so only events carrying
/// our own payment id are accepted -- a concurrent payment must not leak
/// its result into ours.
async fn wait_for_payment_result(
    mut events: lampo_common::chan::UnboundedReceiver<Event>,
    payment_id: &str,
    expected_value_msat: Option<u64>,
    timeout: Option<Duration>,
) -> Result<json::Value, Error> {
    // Regular JSON-RPC calls retain a single deadline for the whole wait.
    // LND-compatible calls have no terminal deadline because their timeout only
    // limits the period before launching an HTLC.
    let deadline = timeout.map(|timeout| Instant::now() + timeout);

    // The receipt lands on `PaymentReceipt` and the hop path on the terminal
    // `PaymentEvent`, so hold the receipt until the payment finishes.
    let mut receipt: Option<(String, Option<String>)> = None;
    let mut successful_path = Vec::new();
    let mut value_msat = 0_u64;
    let mut fee_msat = 0_u64;
    let mut successful_payment_hash = None;

    loop {
        log::warn!(target: "lampod::jsonrpc::offchain", "Waiting for payment event...");
        let event = if let Some(deadline) = deadline {
            tokio::time::timeout_at(deadline, events.recv())
                .await
                .map_err(|_| {
                    Error::Rpc(RpcError {
                        code: -1,
                        message: format!(
                            "payment `{}` did not complete within {}s (no terminal Payment event; \
                             payment status unknown — it may still be retried in the background)",
                            payment_id,
                            timeout.map(|value| value.as_secs()).unwrap_or_default()
                        ),
                        data: None,
                    })
                })?
        } else {
            events.recv().await
        }
        .ok_or(Error::Rpc(RpcError {
            code: -1,
            message: format!("No event received, communication channel dropped"),
            data: None,
        }))?;

        match event {
            Event::Lightning(LightningEvent::PaymentReceipt {
                payment_id: id,
                payment_preimage,
                payer_proof,
            }) if id == payment_id => {
                receipt = Some((payment_preimage, payer_proof));
                // Path success can arrive first. Once we have the receipt
                // and a successful path, the payment is done.
                if !successful_path.is_empty() {
                    let (payment_preimage, payer_proof) = receipt
                        .clone()
                        .map(|(preimage, proof)| (Some(preimage), proof))
                        .unwrap_or((None, None));
                    return Ok(json::to_value(PayResult {
                        state: lampo_common::model::response::PaymentState::Success,
                        path: successful_path,
                        payment_hash: successful_payment_hash,
                        value_msat: expected_value_msat.unwrap_or(value_msat),
                        fee_msat,
                        reason: None,
                        payment_preimage,
                        payer_proof,
                    })?);
                }
            }
            Event::Lightning(LightningEvent::PaymentEvent {
                payment_id: Some(id),
                payment_hash,
                path,
                state,
                reason,
            }) if id == payment_id => {
                if state == lampo_common::model::response::PaymentState::Success {
                    let (path_value_msat, path_fee_msat) = path_value_and_fee(&path);
                    value_msat = value_msat.saturating_add(path_value_msat);
                    fee_msat = fee_msat.saturating_add(path_fee_msat);
                    successful_path.extend(path);
                    successful_payment_hash =
                        successful_payment_hash.or_else(|| payment_hash.clone());
                    // Last-hop `fee_msat` is the intro-node / blinded-path
                    // *fee*, not the payment value (async static-invoice
                    // path: hop_fee=1000, invoice=100000). Waiting until
                    // hop fees sum to the invoice never finishes. ldk-node
                    // treats PaymentSent as terminal and does not compare
                    // hop fees. Fill the reported value from the invoice
                    // when hops do not carry it.
                    if value_msat < expected_value_msat.unwrap_or(0) {
                        value_msat = expected_value_msat.unwrap_or(value_msat);
                    }
                    // PaymentPathSuccessful can race PaymentSent. Hold the
                    // Success until the receipt (preimage) lands so `pay`
                    // does not return without it.
                    if receipt.is_none() {
                        continue;
                    }
                }
                let (payment_preimage, payer_proof) = match receipt {
                    Some((preimage, proof)) => (Some(preimage), proof),
                    None => (None, None),
                };
                return Ok(json::to_value(PayResult {
                    state,
                    path: successful_path,
                    payment_hash: successful_payment_hash.or(payment_hash),
                    value_msat,
                    fee_msat,
                    reason,
                    payment_preimage,
                    payer_proof,
                })?);
            }
            _ => {}
        }
    }
}

pub async fn json_keysend(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `keysend` with request `{:?}`", request);
    let request: KeySend = json::from_value(request.clone())?;
    let destination = request.destination()?;
    let mut events = ctx.handler().events();
    let payment_id = ctx
        .offchain_manager()
        .keysend(destination, request.amount_msat)?;
    // Same id semantics as `pay`: the hex payment hash identifies the
    // payment on the event bus.
    let payment_id = hex::encode(payment_id.0);
    wait_for_payment_result(
        events,
        &payment_id,
        Some(request.amount_msat),
        Some(request.timeout.duration()),
    )
    .await
}

fn path_value_and_fee(path: &[response::PaymentHop]) -> (u64, u64) {
    let value_msat = path.last().map(|hop| hop.hop_fee_msat).unwrap_or(0);
    let total_msat = path
        .iter()
        .fold(0_u64, |total, hop| total.saturating_add(hop.hop_fee_msat));
    (value_msat, total_msat.saturating_sub(value_msat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_retry_deadline_does_not_end_terminal_wait() {
        assert_eq!(
            terminal_wait_timeout(Some(60), Duration::from_secs(120)),
            None
        );
        assert_eq!(
            terminal_wait_timeout(None, Duration::from_secs(120)),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn aggregates_value_and_fees_per_mpp_path() {
        let hop = |fee| response::PaymentHop {
            node_id: String::new(),
            short_channel_id: 0,
            hop_fee_msat: fee,
            cltv_expiry_delta: 0,
            private_hop: false,
        };

        assert_eq!(path_value_and_fee(&[hop(20), hop(400)]), (400, 20));
        assert_eq!(path_value_and_fee(&[hop(30), hop(600)]), (600, 30));
    }
}

fn build_reveal_contact_params(
    ctx: &LampoDaemon,
    offer_str: &str,
    contact_label: Option<String>,
    intro_node: Option<String>,
) -> Result<ContactPaymentParams, Error> {
    let label = contact_label.ok_or_else(|| {
        Error::Rpc(RpcError {
            code: -1,
            message: "reveal_contact requires bolt12.contact_label".into(),
            data: None,
        })
    })?;
    let their_offer = Offer::from_str(offer_str).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("invalid offer: {err:?}"),
            data: None,
        })
    })?;
    let store = ctx.contact_store();

    // If we already know this contact (especially after an inbound payment),
    // reuse their secret and only (re)build our compact payer offer if needed.
    if let Some(contact) = store.get(&label) {
        return contact_params_from_stored(ctx, &their_offer, &contact, intro_node.as_deref());
    }

    let intro_pk = parse_pubkey(&intro_node.ok_or_else(|| {
        Error::Rpc(RpcError {
            code: -1,
            message: "reveal_contact requires bolt12.intro_node for a new contact".into(),
            data: None,
        })
    })?)?;
    let params = ctx
        .offchain_manager()
        .build_contact_payment_params(&their_offer, intro_pk, None)
        .map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: err.to_string(),
                data: None,
            })
        })?;
    let nonce_hex = params
        .nonce
        .as_ref()
        .map(|n| hex::encode(n.as_slice()))
        .ok_or_else(|| {
            Error::Rpc(RpcError {
                code: -1,
                message: "compact payer offer missing nonce".into(),
                data: None,
            })
        })?;
    store
        .remember_outbound(
            &label,
            &their_offer,
            params.secrets.primary_secret(),
            &params.payer_offer,
            &nonce_hex,
        )
        .map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: err.to_string(),
                data: None,
            })
        })?;
    Ok(params)
}

fn maybe_payback_contact_params(
    ctx: &LampoDaemon,
    offer_str: &str,
    label: &str,
    intro_node: Option<String>,
) -> Result<Option<ContactPaymentParams>, Error> {
    let store = ctx.contact_store();
    let Some(contact) = store.get(label) else {
        return Ok(None);
    };
    if contact.remote_offer != offer_str {
        return Ok(None);
    }
    let their_offer = Offer::from_str(offer_str).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("invalid offer: {err:?}"),
            data: None,
        })
    })?;
    // Reuse the inbound secret; mint our compact offer if we do not have one yet.
    Ok(Some(contact_params_from_stored(
        ctx,
        &their_offer,
        &contact,
        intro_node.as_deref(),
    )?))
}

fn contact_params_from_stored(
    ctx: &LampoDaemon,
    their_offer: &Offer,
    contact: &Contact,
    intro_node: Option<&str>,
) -> Result<ContactPaymentParams, Error> {
    let store = ctx.contact_store();
    let secrets = store.secrets_for(contact).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: err.to_string(),
            data: None,
        })
    })?;

    let (payer_offer, nonce) = match (&contact.our_offer, &contact.our_offer_nonce_hex) {
        (Some(offer_s), Some(nonce_hex)) => {
            let offer = Offer::from_str(offer_s).map_err(|err| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!("stored our_offer invalid: {err:?}"),
                    data: None,
                })
            })?;
            (offer, decode_nonce(nonce_hex)?)
        }
        _ => {
            let intro_pk = parse_pubkey(intro_node.ok_or_else(|| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!(
                        "contact `{}` has no stored our_offer; provide bolt12.intro_node (and reveal_contact) to create one for payback",
                        contact.label
                    ),
                    data: None,
                })
            })?)?;
            let (offer, nonce) = ctx
                .offchain_manager()
                .create_compact_payer_offer(intro_pk)
                .map_err(|err| {
                    Error::Rpc(RpcError {
                        code: -1,
                        message: err.to_string(),
                        data: None,
                    })
                })?;
            let nonce_hex = hex::encode(nonce.as_slice());
            store
                .remember_outbound(
                    &contact.label,
                    their_offer,
                    secrets.primary_secret(),
                    &offer,
                    &nonce_hex,
                )
                .map_err(|err| {
                    Error::Rpc(RpcError {
                        code: -1,
                        message: err.to_string(),
                        data: None,
                    })
                })?;
            (offer, nonce)
        }
    };

    Ok(ContactPaymentParams {
        secrets,
        payer_offer,
        nonce: Some(nonce),
    })
}

fn decode_nonce(nonce_hex: &str) -> Result<Nonce, Error> {
    let bytes = hex::decode(nonce_hex).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("invalid nonce hex: {err}"),
            data: None,
        })
    })?;
    Nonce::try_from(bytes.as_slice()).map_err(|_| {
        Error::Rpc(RpcError {
            code: -1,
            message: "contact our_offer_nonce_hex must be 16 bytes".into(),
            data: None,
        })
    })
}

fn parse_pubkey(hex_str: &str) -> Result<PublicKey, Error> {
    let bytes = hex::decode(hex_str).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("invalid intro_node hex: {err}"),
            data: None,
        })
    })?;
    PublicKey::from_slice(&bytes).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("invalid intro_node pubkey: {err}"),
            data: None,
        })
    })
}

pub async fn json_listcontacts(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    let _: ListContacts = json::from_value(request.clone()).unwrap_or(ListContacts {});
    let contacts = ctx
        .contact_store()
        .list()
        .into_iter()
        .map(|c| ContactInfo {
            label: c.label,
            remote_offer: c.remote_offer,
            primary_secret_hex: c.primary_secret_hex,
            our_offer: c.our_offer,
        })
        .collect();
    Ok(json::to_value(Contacts { contacts })?)
}

pub async fn json_addcontact(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    let request: AddContact = json::from_value(request.clone())?;
    let _offer = Offer::from_str(&request.offer).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("invalid offer: {err:?}"),
            data: None,
        })
    })?;
    let primary_secret_hex = if let Some(secret_hex) = request.contact_secret_hex {
        let bytes = hex::decode(&secret_hex).map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("invalid contact_secret_hex: {err}"),
                data: None,
            })
        })?;
        if bytes.len() != 32 {
            return Err(Error::Rpc(RpcError {
                code: -1,
                message: "contact_secret_hex must be 32 bytes".into(),
                data: None,
            }));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let secrets = ContactSecrets::from_remote_secret(ContactSecret::new(arr));
        hex::encode(secrets.primary_secret().as_bytes())
    } else {
        return Err(Error::Rpc(RpcError {
            code: -1,
            message:
                "addcontact currently requires contact_secret_hex (from an inbound BLIP-42 payment)"
                    .into(),
            data: None,
        }));
    };
    let contact = Contact {
        label: request.label.clone(),
        primary_secret_hex,
        remote_offer: request.offer,
        our_offer: None,
        our_offer_nonce_hex: None,
        additional_remote_secrets_hex: Vec::new(),
    };
    ctx.contact_store().upsert(contact.clone()).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: err.to_string(),
            data: None,
        })
    })?;
    Ok(json::to_value(ContactInfo {
        label: contact.label,
        remote_offer: contact.remote_offer,
        primary_secret_hex: contact.primary_secret_hex,
        our_offer: contact.our_offer,
    })?)
}
