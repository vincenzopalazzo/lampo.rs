//! Full feature async JSON RPC 2.0 Server/client with a
//! minimal dependencies footprint.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

pub mod command;
pub mod errors;
pub mod json_rpc2;

use crate::command::Context;
use crate::errors::Error;
use crate::json_rpc2::{Request, Response};

#[derive(Debug, Clone, PartialEq)]
pub enum RPCEvent {
    Listening,
    Connect(i32),
}

pub struct JSONRPCv2<T: Send + Sync + 'static> {
    socket_path: String,
    handler: Arc<Handler<T>>,
}

pub struct Handler<T: Send + Sync + 'static> {
    stop: Cell<bool>,
    rpc_method:
        RefCell<HashMap<String, Arc<dyn Fn(&T, &Value) -> Result<Value, errors::Error> + 'static>>>,
    ctx: Arc<dyn Context<Ctx = T>>,
}

unsafe impl<T: Send + Sync> Sync for Handler<T> {}
unsafe impl<T: Send + Sync> Send for Handler<T> {}

impl<T: Send + Sync + 'static> Handler<T> {
    pub fn new(ctx: Arc<dyn Context<Ctx = T>>) -> Self {
        Handler::<T> {
            stop: Cell::new(false),
            rpc_method: RefCell::new(HashMap::new()),
            ctx,
        }
    }

    pub fn add_method<F>(&self, method: &str, callback: F)
    where
        F: Fn(&T, &Value) -> Result<Value, errors::Error> + 'static,
    {
        self.rpc_method
            .borrow_mut()
            .insert(method.to_owned(), Arc::new(callback));
    }

    pub fn run_callback(&self, req: &Request<Value>) -> Option<Result<Value, errors::Error>> {
        let binding = self.rpc_method.borrow();
        let Some(callback) = binding.get(&req.method) else {
            return Some(Err(errors::RpcError {
                message: format!("method `{}` not found", req.method),
                code: -1,
                data: None,
            }
            .into()));
        };
        let resp = callback(self.ctx(), &req.params);
        Some(resp)
    }

    pub fn has_rpc(&self, method: &str) -> bool {
        self.rpc_method.borrow().contains_key(method)
    }

    fn ctx(&self) -> &T {
        self.ctx.ctx()
    }

    pub fn stop(&self) {
        self.stop.set(true);
    }
}

impl<T: Send + Sync + 'static> JSONRPCv2<T> {
    pub fn new(ctx: Arc<dyn Context<Ctx = T>>, path: &str) -> Result<Self, Error> {
        Ok(Self {
            handler: Arc::new(Handler::new(ctx)),
            socket_path: path.to_owned(),
        })
    }

    pub fn add_rpc<F>(&self, name: &str, callback: F) -> Result<(), ()>
    where
        F: Fn(&T, &Value) -> Result<Value, errors::Error> + 'static,
    {
        if self.handler.has_rpc(name) {
            return Err(());
        }
        self.handler.add_method(name, callback);
        Ok(())
    }

    #[allow(dead_code)]
    fn ctx(&self) -> &T {
        self.handler.ctx()
    }

    async fn handle_connection(&self, mut socket: UnixStream) {
        let mut buffer = vec![0; 1024];

        loop {
            let n = match socket.read(&mut buffer).await {
                Ok(n) if n == 0 => return, // Connection was closed
                Ok(n) => n,
                Err(_) => return, // An error occurred
            };

            let request: Request<Value> = match serde_json::from_slice(&buffer[..n]) {
                Ok(req) => req,
                Err(_) => continue, // Invalid request
            };

            let Some(rpc) = self.handler.run_callback(&request) else {
                continue;
            };
            let response = if let Ok(method) = rpc {
                Response {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(method),
                    error: None,
                }
            } else {
                Response {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: None,
                    error: Some(rpc.err().unwrap().into()),
                }
            };

            let response = serde_json::to_vec(&response).unwrap();
            socket.write_all(&response).await.unwrap();
        }
    }

    pub async fn listen(self) -> io::Result<()> {
        let path = self.socket_path.clone();
        let listnet = UnixListener::bind(path.clone())?;
        while !self.handler.stop.get() {
            let (socket, _) = listnet.accept().await?;

            self.handle_connection(socket).await;
        }
        Ok(())
    }

    pub fn spawn(self) -> io::Result<()> {
        unimplemented!()
    }

    pub fn handler(&self) -> Arc<Handler<T>> {
        self.handler.clone()
    }
}

impl<T: Send + Sync + 'static> Drop for JSONRPCv2<T> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write, os::unix::net::UnixStream, path::Path, str::FromStr, sync::Arc, time::Duration,
    };

    use lampo_common::logger;
    use ntest::timeout;
    use serde_json::Value;

    use crate::{
        command::Context,
        json_rpc2::{Id, Request, Response},
        JSONRPCv2,
    };

    struct DummyCtx;

    impl Context for DummyCtx {
        type Ctx = DummyCtx;

        fn ctx(&self) -> &Self::Ctx {
            self
        }
    }

    #[timeout(9000)]
    fn register_rpc() {
        logger::init("debug", None).unwrap();
        let path = "/tmp/tmp.sock";
        let _ = std::fs::remove_file(path);
        let server = JSONRPCv2::new(Arc::new(DummyCtx), path).unwrap();
        let _ = server.add_rpc("foo", |_: &DummyCtx, request| {
            Ok(serde_json::json!(request))
        });
        let res = server.add_rpc("secon", |_: &DummyCtx, request| {
            Ok(serde_json::json!(request))
        });
        assert!(res.is_ok(), "{:?}", res);

        let handler = server.handler();
        let _ = std::thread::spawn(|| {
            tokio::runtime::Handle::current().block_on(async move { server.listen().await })
        });
        let request = Request::<Value> {
            id: Some(0.into()),
            jsonrpc: String::from_str("2.0").unwrap(),
            method: "foo".to_owned(),
            params: serde_json::Value::Array([].to_vec()),
        };
        let client_worker = std::thread::spawn(move || {
            let buff = serde_json::to_string(&request).unwrap();
            //connect to the socket
            let mut stream = match UnixStream::connect(Path::new("/tmp/tmp.sock")) {
                Err(_) => panic!("server is not running"),
                Ok(stream) => stream,
            };
            log::info!(target: "client", "sending {buff}");
            let _ = stream.write_all(buff.as_bytes()).unwrap();
            let _ = stream.flush().unwrap();
            log::info!(target: "client", "waiting for server response");
            log::info!(target: "client", "read answer from server");
            let resp: Response<Value> = serde_json::from_reader(stream).unwrap();
            log::info!(target: "client", "msg received: {:?}", resp);
            assert_eq!(resp.id, request.id);
            resp
        });

        let client_worker2 = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));

            let request = Request::<Value> {
                id: Some(1.into()),
                jsonrpc: String::from_str("2.0").unwrap(),
                method: "secon".to_owned(),
                params: serde_json::Value::Array([].to_vec()),
            };

            let buff = serde_json::to_string(&request).unwrap();
            let mut stream = match UnixStream::connect(Path::new("/tmp/tmp.sock")) {
                Err(_) => panic!("server is not running"),
                Ok(stream) => stream,
            };
            log::info!(target: "client", "sending {buff}");
            let _ = stream.write_all(buff.as_bytes()).unwrap();
            let _ = stream.flush().unwrap();
            log::info!(target: "client", "waiting for server response");
            log::info!(target: "client", "read answer from server");
            let resp: Response<Value> = serde_json::from_reader(stream).unwrap();
            log::info!(target: "client", "msg received: {:?}", resp);
            resp
        });

        let resp = client_worker.join().unwrap();
        assert_eq!(Some(Id::Str("0".to_owned())), resp.id);
        let resp = client_worker2.join().unwrap();
        assert_eq!(Some(Id::Str("1".to_owned())), resp.id);
        handler.stop();
    }
}
