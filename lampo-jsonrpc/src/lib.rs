//! Full feature JSON RPC 2.0 Server/client with a
//! minimal dependencies footprint.
mod errors;
mod json_rpc2;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::{io, os::unix::net::UnixListener};

use popol::{Sources, Timeout};

use errors::Error;
use serde_json::Value;

use crate::json_rpc2::{Request, Response};

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum RPCEvent {
    Listening,
}

type Callback = fn(&Value) -> Value;

pub struct JSONRPCv2 {
    socket_path: String,
    sources: Sources<RPCEvent>,
    rpc_method: HashMap<String, Box<Callback>>,
    socket: Arc<UnixListener>,
}

impl JSONRPCv2 {
    pub fn new(path: &str) -> Result<Self, Error> {
        let listnet = UnixListener::bind(path)?;
        let sources = Sources::<RPCEvent>::new();
        let socket = Arc::new(listnet);
        Ok(Self {
            sources,
            socket: socket.clone(),
            socket_path: path.to_owned(),
            rpc_method: HashMap::new(),
        })
    }

    pub fn add_rpc(&mut self, name: &str, callback: Callback) -> Result<(), ()> {
        if self.rpc_method.contains_key(name) {
            return Err(());
        }
        self.rpc_method.insert(name.to_owned(), Box::new(callback));
        Ok(())
    }

    pub fn listen(mut self) -> io::Result<()> {
        self.socket.set_nonblocking(true)?;
        self.sources
            .register(RPCEvent::Listening, &self.socket, popol::interest::ALL);

        log::debug!("starting server");
        let mut events = vec![];
        loop {
            self.sources.poll(&mut events, Timeout::Never).unwrap();
            log::info!("pooling but event size: {}", events.len());
            for event in events.iter() {
                log::info!("event {:?}", event);
                match &event.key {
                    RPCEvent::Listening => {
                        for incoming in self.socket.incoming() {
                            log::info!("incoming connection {:?}", incoming);
                            let Ok(mut client) = incoming else {
                                log::error!("error found {:?}", incoming);
                                break;
                            };
                            client.set_nonblocking(true)?;
                            let mut buff = String::new();
                            client.read_to_string(&mut buff).unwrap();

                            let requ: Request<Value> = serde_json::from_str(&buff).unwrap();
                            let callback = self.rpc_method.get(&requ.method).unwrap();
                            let resp = callback(&requ.params);
                            let resp = Response {
                                id: requ.id.clone().unwrap(),
                                jsonrpc: Some(requ.jsonrpc.clone()),
                                result: Some(resp),
                                error: None,
                            };
                            client.set_nonblocking(true);
                            let buff = serde_json::to_string(&resp).unwrap();
                            log::info!("write to socket {buff}");
                            client.write_all(buff.as_bytes()).unwrap();
                        }
                    }
                }
            }
        }
    }

    pub fn spawn(self) -> JoinHandle<io::Result<()>> {
        std::thread::spawn(move || self.listen())
    }
}

impl Drop for JSONRPCv2 {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        path::Path,
        str::FromStr,
        time::Duration,
    };

    use lampo_common::logger;
    use serde_json::Value;

    use crate::{json_rpc2::Request, JSONRPCv2};

    #[test]
    fn register_rpc() {
        logger::init(log::Level::Debug).unwrap();
        let mut server = JSONRPCv2::new("/tmp/tmp.sock").unwrap();
        let res = server.add_rpc("foo", |request| serde_json::json!({}));
        assert!(res.is_ok(), "{:?}", res);

        let worker = server.spawn();
        let request = Request::<Value> {
            id: Some(0.into()),
            jsonrpc: String::from_str("2.0").unwrap(),
            method: "foo".to_owned(),
            params: serde_json::Value::Array([].to_vec()),
        };
        let bugg = serde_json::to_string(&request).unwrap();
        // Connect to socket
        let _ = match UnixStream::connect(Path::new("/tmp/tmp.sock")) {
            Err(_) => panic!("server is not running"),
            Ok(mut stream) => {
                stream.set_nonblocking(true);
                stream.write_all(bugg.as_bytes());
                let mut string = String::new();
                stream.read_to_string(&mut string);
                log::info!("client side msg received: {string}");
            }
        };
        let _ = worker.join();
    }
}
