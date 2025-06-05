use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::offchain::{json_decode, json_invoice, json_offer, json_pay};

use crate::{post, AppState, ResultJson};

post!(invoice, request: request::GenerateInvoice, response: response::Invoice);
post!(offer, request: request::GenerateOffer, response: response::Offer);
// FIXME(vincenzopalazzo): the decode should be generic over any kind of string
post!(decode, request: request::DecodeInvoice, response: response::Bolt11InvoiceInfo);
post!(pay, request: request::Pay, response: response::PayResult);
