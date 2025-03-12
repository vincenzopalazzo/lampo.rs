use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::offchain::{json_decode_invoice, json_invoice, json_pay};

use crate::{post, AppState, ResultJson};

post!(json_invoice, request: request::GenerateInvoice, response: response::Invoice);
// FIXME(vincenzopalazzo): the decode should be generic over any kind of string
post!(json_decode_invoice, request: request::DecodeInvoice, response: response::Bolt11InvoiceInfo);
post!(json_pay, request: request::Pay, response: response::PayResult);
