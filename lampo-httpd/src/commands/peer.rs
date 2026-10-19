use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};
use paste::paste;

use lampo_common::json;
use lampo_common::model::{request, response};
use lampod::jsonrpc::channels::*;
use lampod::jsonrpc::open_channel::*;
use lampod::jsonrpc::peer_control::*;

use crate::{post, AppState, ResultJson};

post!(connect, request: request::Connect, response: request::Connect);
post!(close, request: request::CloseChannel, response: response::CloseChannel);
post!(channels, request: json::Value, response: json::Value);
post!(peers, response: response::Peers);
post!(fundchannel, request: request::OpenChannel, response: json::Value);
post!(splicein, request: request::SpliceIn, response: json::Value);
post!(spliceout, request: request::SpliceOut, response: json::Value);
post!(bumpchannelfee, request: request::BumpChannelFee, response: json::Value);
