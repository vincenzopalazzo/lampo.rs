mod args;

use std::process::exit;

use elite_rpc::transport::curl::HttpTransport;
use elite_rpc::transport::TransportMethod;
use elite_rpc::EliteRPC;
use radicle_term as term;

//use lampo_common::error;
//use lampo_common::json;

use crate::args::LampoCliArgs;

fn main() -> error::Result<()> {
    let args = match args::parse_args() {
        Ok(args) => args,
        Err(err) => {
            term::error(format!("{err}"));
            exit(1);
        }
    };
    let resp = run(args);
    log::info!("{:?}", resp);
    match resp {
        Ok(resp) => {
            term::print(json::to_string_pretty(&resp)?);
        }
        Err(err) => {
            term::error(format!("{err}"));
            exit(1);
        }
    }
    Ok(())
}

// FIXME: we should be able to support differen kind of error in here.
fn run(args: LampoCliArgs) -> error::Result<json::Value> {
    let inner: EliteRPC<HttpTransport<RestProtocol>, RestProtocol> =
        EliteRPC::new(&format!("{}", args.url))?;
    let method = args.method;
    let args = args.args;
    inner.call(TransportMethod::Post(method), &json::to_value(args)?)
}

// FIXME: this need to be refactored somewhere
/// Rest RPC module written with elite RPC.
///
/// Author: Vincenzo Palazzo <vincenzopalazzodev@gmail.com>
use std::io::Cursor;

use elite_rpc::protocol::Protocol;

use lampo_common::error;
use lampo_common::json;

#[derive(Clone)]
pub struct RestProtocol;

impl Protocol for RestProtocol {
    type InnerType = json::Value;

    fn new() -> error::Result<Self> {
        Ok(Self)
    }

    fn to_request(
        &self,
        url: &str,
        req: &Self::InnerType,
    ) -> error::Result<(String, Self::InnerType)> {
        Ok((url.to_string(), req.clone()))
    }

    fn from_request(
        &self,
        content: &[u8],
        _: std::option::Option<elite_rpc::protocol::Encoding>,
    ) -> error::Result<<Self as elite_rpc::protocol::Protocol>::InnerType> {
        let cursor = Cursor::new(content);
        let response: json::Value = json::from_reader(cursor)?;
        Ok(response)
    }
}
