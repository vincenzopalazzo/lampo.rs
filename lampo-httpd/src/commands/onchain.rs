use paperclip::actix::web;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::response;
use lampod::jsonrpc::onchain::{json_funds, json_new_addr};

use crate::{post, AppState, ResultJson};

post!(new_addr, response: response::NewAddress);
post!(funds, response: json::Value);
