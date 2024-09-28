use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::channels::{json_close_channel, json_list_channels};
use lampod::jsonrpc::open_channel::json_open_channel;
use lampod::jsonrpc::peer_control::json_connect;

use crate::{post, AppState, ResultJson};

post!(json_connect, request: request::Connect, response: request::Connect);
post!(json_close_channel, response: response::CloseChannel);
post!(json_list_channels, request: json::Value, response: json::Value);
post!(json_open_channel, request: request::OpenChannel, response: json::Value);
