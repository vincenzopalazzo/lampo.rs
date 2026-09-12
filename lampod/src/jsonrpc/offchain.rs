//! Offchain RPC methods
use std::str::FromStr;
use std::time::Duration;

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::ldk;
use lampo_common::ldk::offers::contacts::{ContactSecret, ContactSecrets};
use lampo_common::ldk::offers::nonce::Nonce;
use lampo_common::ldk::offers::offer;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::model::request::AddContact;
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::request::GenerateOffer;
use lampo_common::model::request::KeySend;
use lampo_common::model::request::ListContacts;
use lampo_common::model::request::Pay;
use lampo_common::model::response::ContactInfo;
use lampo_common::model::response::Contacts;
use lampo_common::model::response::PayResult;
use lampo_common::model::response::{self, Decode};
use lampo_common::model::response::{Bolt11InvoiceInfo, Bolt12InvoiceInfo, Invoice};
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
    let manager = ctx.channel_manager().manager();
    let mut offer_builder = manager
        .create_offer_builder()
        .map_err(|err| crate::rpc_error!("{:?}", err))?;

    if let Some(description) = request.description {
        offer_builder = offer_builder.description(description);
    }

    if let Some(amount_msat) = request.amount_msat {
        offer_builder = offer_builder.amount_msats(amount_msat);
    }

    let offer: response::Offer = offer_builder
        .build()
        // FIXME: implement display error on top of the bolt12 error
        .map_err(|err| crate::rpc_error!("{:?}", err))?
        .into();
    log::debug!("Generated offer: {:?}", offer);
    Ok(json::to_value(&offer)?)
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
            expiry_time: Some(invoice.expiry_time().as_millis() as u64),
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

    let payment_id = if let Ok(_) = offer::Offer::from_str(&request.invoice_str) {
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
            contact_params,
        )?
    } else {
        log::debug!(
            "Paying invoice with bolt11 invoice: {}",
            request.invoice_str
        );
        ctx.offchain_manager()
            .pay_invoice(&request.invoice_str, request.amount)?
    };
    // The event bus broadcasts to every subscriber, so a concurrent `pay` would
    // otherwise see this payment's result -- and now its preimage and payer
    // proof too. Only accept events carrying our own payment id.
    let payment_id = hex::encode(payment_id.0);
    wait_for_payment_result(events, &payment_id, request.timeout.duration()).await
}

/// Hold the `PaymentReceipt` (preimage, payer proof) until the terminal
/// `PaymentEvent` for `payment_id` arrives, and build the `PayResult`.
/// The event bus broadcasts to every subscriber, so only events carrying
/// our own payment id are accepted -- a concurrent payment must not leak
/// its result into ours.
async fn wait_for_payment_result(
    mut events: lampo_common::chan::UnboundedReceiver<Event>,
    payment_id: &str,
    timeout: Duration,
) -> Result<json::Value, Error> {
    // Single deadline for the whole RPC wait. The event bus is broadcast, so
    // unrelated events must not reset the timer — only the terminal
    // `PaymentEvent` for `payment_id` completes the call (success or failure).
    // If that event never arrives, stop waiting after `timeout` instead of
    // blocking forever; the payment itself may still be retried in the background.
    let deadline = Instant::now() + timeout;

    // The receipt lands on `PaymentReceipt` and the hop path on the terminal
    // `PaymentEvent`, so hold the receipt until the payment finishes.
    let mut receipt: Option<(String, Option<String>)> = None;

    loop {
        log::warn!(target: "lampod::jsonrpc::offchain", "Waiting for payment event...");
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .map_err(|_| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!(
                        "payment `{}` did not complete within {}s (no terminal Payment event; \
                         payment status unknown — it may still be retried in the background)",
                        payment_id,
                        timeout.as_secs()
                    ),
                    data: None,
                })
            })?
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
            }
            Event::Lightning(LightningEvent::PaymentEvent {
                payment_id: Some(id),
                payment_hash,
                path,
                state,
                reason: _,
            }) if id == payment_id => {
                let (payment_preimage, payer_proof) = match receipt {
                    Some((preimage, proof)) => (Some(preimage), proof),
                    None => (None, None),
                };
                return Ok(json::to_value(PayResult {
                    state,
                    path,
                    payment_hash,
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
    wait_for_payment_result(events, &payment_id, request.timeout.duration()).await
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
