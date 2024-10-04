use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::offchain::*;

use crate::{post, AppState, ResultJson};

post!(invoice, request: request::GenerateInvoice, response: response::Invoice);
post!(decode, request: request::DecodeInvoice, response: response::InvoiceInfo);
post!(pay, request: request::Pay, response: response::PayResult);
