use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::inventory::*;
use lampod::jsonrpc::onchain::*;

use crate::{post, AppState, ResultJson};

post!(getinfo, response: response::GetInfo);
post!(networkchannels, request: json::Value, response: response::NetworkChannels);
post!(funds, request: json::Value, response: response::Utxos);
