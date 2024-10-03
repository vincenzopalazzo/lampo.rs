use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::inventory::{get_info, json_network_channels};

use crate::{post, AppState, ResultJson};

post!(get_info, response: response::GetInfo);
post!(json_network_channels, request: json::Value, response: response::NetworkChannels);
