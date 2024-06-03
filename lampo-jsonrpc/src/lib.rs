//! Full feature async JSON RPC 2.0 Server/client with a
//! minimal dependencies footprint.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread::JoinHandle;

// FIXME: use mio for a better platform support.
use popol::{Event, Sources, Timeout};
use serde_json::Value;

pub mod command;
pub mod errors;
pub mod json_rpc2;

use command::Context;

use crate::errors::Error;
use crate::json_rpc2::{Request, Response};

#[derive(Debug, Clone, PartialEq)]
pub enum RPCEvent {
    Accept,
    Connect,
}

pub struct JSONRPCv2<T: Send + Sync + 'static> {
    socket_path: String,
    sources: Sources<RPCEvent>,
    open_streams: HashMap<i32, UnixStream>,
    response_queue: HashMap<i32, Response<Value>>,
    socket: UnixListener,
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
        let listnet = UnixListener::bind(path)?;
        let sources = Sources::<RPCEvent>::new();
        Ok(Self {
            sources,
            socket: listnet,
            handler: Arc::new(Handler::new(ctx)),
            socket_path: path.to_owned(),
            open_streams: HashMap::new(),
            response_queue: HashMap::new(),
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

    fn read(&mut self, event: &mut Event<RPCEvent>) -> io::Result<()> {
        log::trace!("read from connection");
        let fd = event.as_raw_fd();
        // SAFETY: in this case the socket stream should be created
        let stream = self.open_streams.get_mut(&fd.clone()).unwrap();
        log::trace!("start reading");
        // Nb. Since `poll`, which this reactor is based on, is *level-triggered*,
        // we will be notified again if there is still data to be read on the socket.
        // Hence, there is no use in putting this socket read in a loop, as the second
        // invocation would likely block.
        let mut buff = vec![0; 1064];
        let resp = loop {
            match stream.read(&mut buff) {
                Ok(count) => {
                    buff.truncate(count);
                    if count > 0 {
                        log::info!(target: "jsonrpc", "buffer read {}", String::from_utf8(buff.to_vec()).unwrap());
                        // Put this inside the unfinish queue
                        let Ok(requ) = serde_json::from_slice::<Request<Value>>(&buff) else {
                            log::warn!(target: "jsonrpc", "looks like that the json is not fully read ` {}`", String::from_utf8(buff.to_vec()).unwrap());
                            // Usually this mean that we was too fast in reading and the sender too low
                            continue;
                        };
                        log::trace!(target: "jsonrpc", "request {:?}", requ);
                        let Some(resp) = self.handler.run_callback(&requ) else {
                            log::error!(target: "jsonrpc", "`{}` not found!", requ.method);
                            return Ok(());
                        };
                        // FIXME; the id in the JSON RPC can be null!
                        let response = match resp {
                            Ok(result) => Response {
                                id: requ.id.clone().unwrap(),
                                jsonrpc: requ.jsonrpc.to_owned(),
                                result: Some(result),
                                error: None,
                            },
                            Err(err) => Response {
                                result: None,
                                error: Some(err.into()),
                                id: requ.id.unwrap().clone(),
                                jsonrpc: requ.jsonrpc.clone(),
                            },
                        };
                        break response;
                    } else {
                        log::info!("Reading is not finished, so keep reading");
                        event.source.unset(popol::interest::READ);
                        return Ok(());
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    log::trace!("reading is blocking");
                    // This shouldn't normally happen, since this function is only called
                    // when there's data on the socket. We leave it here in case external
                    // conditions change.
                    return Ok(());
                }
                Err(err) => {
                    log::error!("{:?}", err);
                    self.sources.unregister(&event.key);
                    return Err(err);
                }
            }
        };

        log::trace!(target: "jsonrpc", "send response: `{:?}`", resp);
        self.response_queue.insert(fd, resp);
        event.source.set(popol::interest::WRITE);
        Ok(())
    }

    pub fn listen(mut self) -> io::Result<()> {
        self.socket.set_nonblocking(true)?;
        self.sources
            .register(RPCEvent::Accept, &self.socket, popol::interest::READ);
        log::info!(target: "jsonrpc", "starting server on {}", self.socket_path);
        let mut events = vec![];
        while !self.handler.stop.get() {
            // Blocking while we are waiting new events!
            self.sources.poll(&mut events, Timeout::Never)?;
            for mut event in events.drain(..) {
                match &event.key {
                    RPCEvent::Accept => loop {
                        let accept = self.socket.accept();
                        if let Err(err) = accept {
                            if err.kind() == ErrorKind::WouldBlock {
                                log::trace!("accepting the connection is blocking");
                                break;
                            }
                            return Err(err);
                        }
                        log::info!("Accepting connection: `{:?}`", accept);
                        let stream = accept?.0;
                        self.sources.register(
                            RPCEvent::Connect,
                            &stream,
                            popol::interest::READ | popol::interest::WRITE,
                        );
                        self.open_streams.insert(stream.as_raw_fd(), stream);
                        break;
                    },
                    RPCEvent::Connect if event.is_readable() => {
                        self.read(&mut event)?;
                    }
                    // FIXME: convert all the code inside a `self.write`
                    RPCEvent::Connect if event.is_writable() => {
                        let fd = event.as_raw_fd();
                        // SAFETY: we must have the response for this fd.
                        let resp = self.response_queue.remove(&fd).unwrap();
                        // SAFETY: we much have a stream for this fd.
                        let mut stream = self.open_streams.remove(&event.as_raw_fd()).unwrap();
                        // SAFETY: the resp should be a valid json.
                        let buff = serde_json::to_string(&resp).unwrap();
                        log::debug!("writing the response `{buff}`");
                        if let Err(err) = stream.write_all(buff.as_bytes()) {
                            if err.kind() != ErrorKind::WouldBlock {
                                return Err(err);
                            }
                            log::info!("writing is blocking");
                            continue;
                        }
                        match stream.flush() {
                            // In this case, we've written all the data, we
                            // are no longer interested in writing to this
                            // socket.
                            Ok(()) => {
                                log::trace!("writing ended");
                                event.source.unset(popol::interest::WRITE);
                                event.source.set(popol::interest::READ);
                                self.sources.unregister(&event.key);
                                continue;
                            }
                            // In this case, the write couldn't complete. Set
                            // our interest to `WRITE` to be notified when the
                            // socket is ready to write again.
                            Err(err)
                                if [io::ErrorKind::WouldBlock, io::ErrorKind::WriteZero]
                                    .contains(&err.kind()) =>
                            {
                                log::info!("writing return an error: {:?}", err);
                                event.source.set(popol::interest::READ);
                                break;
                            }
                            Err(err) => {
                                return Err(err);
                            }
                        }
                    }
                    RPCEvent::Connect => {
                        if event.is_hangup() || event.is_error() {
                            log::error!(target: "jsonrpc", "an error occurs: {:?}", event);
                            continue;
                        }

                        if event.is_invalid() {
                            log::warn!(target: "jsonrpc", "event invalid: {:?}", event);
                            self.sources.unregister(&event.key);
                            continue;
                        }
                    }
                }
            }
        }
        log::info!("stopping the server");
        Ok(())
    }

    pub fn handler(&self) -> Arc<Handler<T>> {
        self.handler.clone()
    }

    pub fn spawn(self) -> JoinHandle<io::Result<()>> {
        std::thread::spawn(move || self.listen())
    }
}

impl<T: Send + Sync + 'static> Drop for JSONRPCv2<T> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path).unwrap();
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

    #[test]
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
        let worker = server.spawn();
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
            assert_eq!(resp.id, request.id.unwrap());
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
        assert_eq!(Id::Str("0".to_owned()), resp.id);
        let resp = client_worker2.join().unwrap();
        assert_eq!(Id::Str("1".to_owned()), resp.id);
        handler.stop();
    }
}
