//! Opt-in gate for async-payment onion messages.
//!
//! LDK's [`ChannelManager`] implements [`AsyncPaymentsMessageHandler`], so
//! wiring it into [`OnionMessenger`] by default would make every node
//! process `OfferPaths`, static invoices, and held-HTLC messages. Lampo
//! keeps that off until the operator sets `async-payments-role` or
//! installs recipient paths.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use lampo_common::ldk::blinded_path::message::AsyncPaymentsContext;
use lampo_common::ldk::onion_message::async_payments::{
    AsyncPaymentsMessage, AsyncPaymentsMessageHandler, HeldHtlcAvailable, OfferPaths,
    OfferPathsRequest, ReleaseHeldHtlc, ServeStaticInvoice, StaticInvoicePersisted,
};
use lampo_common::ldk::onion_message::messenger::{
    MessageSendInstructions, Responder, ResponseInstruction,
};
use lampo_common::types::{LampoArcChannelManager, LampoChainMonitor};

use crate::utils::logger::LampoLogger;

/// Forwards async-payment onion messages only while [`Self::enabled`] is set.
pub struct AsyncPaymentsHandler {
    inner: Arc<LampoArcChannelManager<LampoChainMonitor, LampoLogger>>,
    enabled: Arc<AtomicBool>,
}

impl AsyncPaymentsHandler {
    pub fn new(
        inner: Arc<LampoArcChannelManager<LampoChainMonitor, LampoLogger>>,
        enabled: Arc<AtomicBool>,
    ) -> Self {
        Self { inner, enabled }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

impl AsyncPaymentsMessageHandler for AsyncPaymentsHandler {
    fn handle_offer_paths_request(
        &self,
        message: OfferPathsRequest,
        context: AsyncPaymentsContext,
        responder: Option<Responder>,
    ) -> Option<(OfferPaths, ResponseInstruction)> {
        if !self.is_enabled() {
            log::debug!(
                target: "lampo",
                "ignoring OfferPathsRequest: async payments are disabled"
            );
            return None;
        }
        self.inner
            .handle_offer_paths_request(message, context, responder)
    }

    fn handle_offer_paths(
        &self,
        message: OfferPaths,
        context: AsyncPaymentsContext,
        responder: Option<Responder>,
    ) -> Option<(ServeStaticInvoice, ResponseInstruction)> {
        if !self.is_enabled() {
            log::debug!(
                target: "lampo",
                "ignoring OfferPaths: async payments are disabled"
            );
            return None;
        }
        self.inner.handle_offer_paths(message, context, responder)
    }

    fn handle_serve_static_invoice(
        &self,
        message: ServeStaticInvoice,
        context: AsyncPaymentsContext,
        responder: Option<Responder>,
    ) {
        if !self.is_enabled() {
            log::debug!(
                target: "lampo",
                "ignoring ServeStaticInvoice: async payments are disabled"
            );
            return;
        }
        self.inner
            .handle_serve_static_invoice(message, context, responder)
    }

    fn handle_static_invoice_persisted(
        &self,
        message: StaticInvoicePersisted,
        context: AsyncPaymentsContext,
    ) {
        if !self.is_enabled() {
            log::debug!(
                target: "lampo",
                "ignoring StaticInvoicePersisted: async payments are disabled"
            );
            return;
        }
        self.inner.handle_static_invoice_persisted(message, context)
    }

    fn handle_held_htlc_available(
        &self,
        message: HeldHtlcAvailable,
        context: AsyncPaymentsContext,
        responder: Option<Responder>,
    ) -> Option<(ReleaseHeldHtlc, ResponseInstruction)> {
        if !self.is_enabled() {
            log::debug!(
                target: "lampo",
                "ignoring HeldHtlcAvailable: async payments are disabled"
            );
            return None;
        }
        self.inner
            .handle_held_htlc_available(message, context, responder)
    }

    fn handle_release_held_htlc(&self, message: ReleaseHeldHtlc, context: AsyncPaymentsContext) {
        if !self.is_enabled() {
            log::debug!(
                target: "lampo",
                "ignoring ReleaseHeldHtlc: async payments are disabled"
            );
            return;
        }
        self.inner.handle_release_held_htlc(message, context)
    }

    fn release_pending_messages(&self) -> Vec<(AsyncPaymentsMessage, MessageSendInstructions)> {
        if !self.is_enabled() {
            // Leave queued messages on the channel manager so a later
            // opt-in (e.g. `setasyncinvoicepaths`) can flush them.
            return Vec::new();
        }
        self.inner.release_pending_messages()
    }
}
