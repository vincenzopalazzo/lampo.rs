//! Full feature async JSON RPC 2.0 Server/client with a
//! minimal dependencies footprint.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixListener;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::{Arc, Mutex};
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
    socket: UnixListener,
    handler: Arc<Handler<T>>,
    // FIXME: should be not the name but the fd int as key?
    pub(crate) conn: HashMap<String, UnixStream>,
    conn_queue: Mutex<Cell<HashMap<String, VecDeque<Response<Value>>>>>,
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
            conn: HashMap::new(),
            conn_queue: Mutex::new(Cell::new(HashMap::new())),
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

    pub fn add_connection(&mut self, stream: UnixStream) {
        let res = stream.set_nonblocking(true);
        debug_assert!(res.is_ok());
        log::trace!("register a new connection listener");
    }

    pub fn send_resp(&self, key: String, resp: Response<Value>) {
        let queue = self.conn_queue.lock().unwrap();

        let mut conns = queue.take();
        log::debug!(target: "jsonrpc", "{:?}", conns);
        if conns.contains_key(&key) {
            let Some(queue) = conns.get_mut(&key) else {
                panic!("queue not found");
            };
            queue.push_back(resp);
        } else {
            let mut q = VecDeque::new();
            q.push_back(resp);
            conns.insert(key, q);
        }
        log::debug!(target: "jsonrpc", "{:?}", conns);
        queue.set(conns);
    }

    pub fn pop_resp(&self, key: String) -> Option<Response<Value>> {
        let queue = self.conn_queue.lock().unwrap();

        let mut conns = queue.take();
        if !conns.contains_key(&key) {
            return None;
        }
        let Some(q) = conns.get_mut(&key) else {
            return None;
        };
        let resp = q.pop_front();
        queue.set(conns);
        resp
    }

    #[allow(dead_code)]
    fn ctx(&self) -> &T {
        self.handler.ctx()
    }

    fn write(&self, event: Event<RPCEvent>, towrite: &mut Vec<Response<Value>>) -> io::Result<()> {
        unimplemented!()
    }

    fn read(&mut self, event: &mut Event<RPCEvent>) -> io::Result<()> {
        log::trace!("read from connection");
        let stream = self.open_streams.get_mut(&event.as_raw_fd()).unwrap();
        log::trace!("start reading");
        let mut buff = vec![0; 1024]; // FIXME: make this variable
                                      // Nb. Since `poll`, which this reactor is based on, is *level-triggered*,
                                      // we will be notified again if there is still data to be read on the socket.
                                      // Hence, there is no use in putting this socket read in a loop, as the second
                                      // invocation would likely block.
        let resp = match stream.read(&mut buff) {
            Ok(count) => {
                if count > 0 {
                    buff.truncate(count);
                    log::info!(target: "jsonrpc", "buffer read {}", String::from_utf8(buff.to_vec()).unwrap());
                    let requ: Request<Value> = serde_json::from_slice(&buff)
                        .map_err(|err| io::Error::new(io::ErrorKind::Other, format!("{err}")))?;
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
                    response
                } else {
                    log::info!("connection close");
                    self.open_streams.remove(&event.as_raw_fd());
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
        };

        log::trace!(target: "jsonrpc", "send response: `{:?}`", resp);
        let buff = serde_json::to_string(&resp).unwrap();
        if let Err(err) = stream.write_all(buff.as_bytes()) {
            if err.kind() != ErrorKind::WouldBlock {
                return Err(err);
            }
            log::info!("writing is blocking");
            return Ok(());
        }
        event.source.set(popol::interest::WRITE);
        self.open_streams.remove(&event.as_raw_fd());
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
                    RPCEvent::Connect if event.is_writable() => {
                        let stream = self.open_streams.get_mut(&event.as_raw_fd()).unwrap();
                        match stream.flush() {
                            // In this case, we've written all the data, we
                            // are no longer interested in writing to this
                            // socket.
                            Ok(()) => {
                                log::trace!("reading ended");
                                event.source.unset(popol::interest::WRITE);
                            }
                            // In this case, the write couldn't complete. Set
                            // our interest to `WRITE` to be notified when the
                            // socket is ready to write again.
                            Err(err)
                                if [io::ErrorKind::WouldBlock, io::ErrorKind::WriteZero]
                                    .contains(&err.kind()) =>
                            {
                                log::info!("reading return an error: {:?}", err);
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

        let _ = worker.join();
    }
}
