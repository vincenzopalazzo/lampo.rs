use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::offchain::{
    json_cancelfetched, json_decode, json_fetchinvoice, json_holdclaim, json_holdfail,
    json_holdinvoice, json_invoice, json_listholds, json_offer, json_pay, json_payfetched,
};

use crate::{post, AppState, ResultJson};

post!(invoice, request: request::GenerateInvoice, response: response::Invoice);
post!(offer, request: request::GenerateOffer, response: response::Offer);
// FIXME(vincenzopalazzo): the decode should be generic over any kind of string
post!(decode, request: request::DecodeInvoice, response: response::Decode);
post!(pay, request: request::Pay, response: response::PayResult);
post!(fetchinvoice, request: request::FetchInvoice, response: response::FetchInvoiceResult);
post!(payfetched, request: request::PayFetched, response: response::PayResult);
post!(cancelfetched, request: request::CancelFetched, response: response::CancelFetchedResult);
post!(holdinvoice, request: request::HoldInvoice, response: response::HoldInvoiceResult);
post!(holdclaim, request: request::HoldClaim, response: response::HoldClaimResult);
post!(holdfail, request: request::HoldFail, response: response::HoldFailResult);
post!(listholds, request: request::ListHolds, response: response::ListHoldsResult);
