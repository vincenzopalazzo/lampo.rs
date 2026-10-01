use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::phoenix_lsp::{
    json_phoenixlsp_dnsaddress, json_phoenixlsp_info, json_phoenixlsp_recordpurchase,
};

use crate::{post, AppState, ResultJson};

post!(phoenixlsp_info, path: "/phoenixlsp-info", response: response::PhoenixLspInfo);
post!(phoenixlsp_dnsaddress, path: "/phoenixlsp-dnsaddress", request: request::PhoenixLspDnsAddress, response: response::PhoenixLspDnsAddress);
post!(phoenixlsp_recordpurchase, path: "/phoenixlsp-recordpurchase", request: request::PhoenixLspRecordPurchase, response: response::PhoenixLspPurchase);
