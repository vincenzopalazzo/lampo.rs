use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::onchain::{json_estimate_fees, json_funds, json_new_addr};

use crate::{post, AppState, ResultJson};

post!(new_addr, request: request::NewAddress, response: response::NewAddress);
post!(funds, response: json::Value);
