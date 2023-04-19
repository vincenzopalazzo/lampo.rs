//! Full feature JSON RPC 2.0 Server/client with a
//! minimal dependencies footprint.
mod errors;
mod json_rpc2;

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, LineWriter, Read, Write};
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::{io, os::unix::net::UnixListener};

use popol::{Sources, Timeout};

use errors::Error;
use serde_json::Value;

use crate::json_rpc2::{Request, Response};

#[derive(Debug, Clone, PartialEq)]
pub enum RPCEvent {
    Listening,
    Connect(String),
}

type Callback = fn(&Value) -> Value;

pub struct JSONRPCv2 {
    socket_path: String,
    sources: Sources<RPCEvent>,
    rpc_method: HashMap<String, Box<Callback>>,
    socket: UnixListener,
    stop: bool,
    pub(crate) conn: HashMap<String, UnixStream>,
    conn_queue: Mutex<Cell<HashMap<String, VecDeque<Response<Value>>>>>,
}

impl JSONRPCv2 {
    pub fn new(path: &str) -> Result<Self, Error> {
        let listnet = UnixListener::bind(path)?;
        let sources = Sources::<RPCEvent>::new();
        Ok(Self {
            sources,
            socket: listnet,
            stop: false,
            rpc_method: HashMap::new(),
            socket_path: path.to_owned(),
            conn: HashMap::new(),
            conn_queue: Mutex::new(Cell::new(HashMap::new())),
        })
    }

    pub fn add_rpc(&mut self, name: &str, callback: Callback) -> Result<(), ()> {
        if self.rpc_method.contains_key(name) {
            return Err(());
        }
        self.rpc_method.insert(name.to_owned(), Box::new(callback));
        Ok(())
    }

    pub fn add_connection(&mut self, key: &SocketAddr, stream: UnixStream) {
        let path = if let Some(path) = key.as_pathname() {
            path.to_str().unwrap()
        } else {
            "unnamed"
        };
        let res = stream.set_nonblocking(true);
        debug_assert!(res.is_ok());
        let event = RPCEvent::Connect(path.to_string());
        self.sources.register(event, &stream, popol::interest::ALL);
        self.conn.insert(path.to_owned(), stream);
    }

    pub fn send_resp(&self, key: String, resp: Response<Value>) {
        let mut queue = self.conn_queue.lock().unwrap().take();
        log::debug!("{:?}", queue);
        if queue.contains_key(&key) {
            let Some(queue) = queue.get_mut(&key) else {
                panic!("queue not found");
            };
            queue.push_back(resp);
        } else {
            let mut q = VecDeque::new();
            q.push_back(resp);
            queue.insert(key, q);
        }
        log::debug!("{:?}", queue);
        self.conn_queue.lock().unwrap().set(queue);
    }

    pub fn pop_resp(&self, key: String) -> Option<Response<Value>> {
        let mut queue = self.conn_queue.lock().unwrap().take();
        if !queue.contains_key(&key) {
            return None;
        }
        let Some(q) = queue.get_mut(&key) else {
            return None;
        };
        let resp = q.pop_front();
        self.conn_queue.lock().unwrap().set(queue);
        resp
    }

    pub fn listen(mut self) -> io::Result<()> {
        self.socket.set_nonblocking(true)?;
        self.sources
            .register(RPCEvent::Listening, &self.socket, popol::interest::READ);

        log::debug!("starting server");
        let mut events = vec![];
        while !self.stop {
            // Blocking while we are waiting new events!
            self.sources.poll(&mut events, Timeout::Never)?;

            for mut event in events.drain(..) {
                log::trace!("event {:?}", event);
                match &event.key {
                    RPCEvent::Listening => {
                        let conn = self.socket.accept();
                        let Ok((stream, addr)) = conn else {
                            if let Err(err) = &conn {
                                if err.kind() == ErrorKind::WouldBlock {
                                    break;
                                }
                            }
                            log::error!("fail to accept the connection: {:?}", conn);
                            continue;
                        };
                        self.add_connection(&addr, stream);
                    }
                    RPCEvent::Connect(addr) => {
                        if event.is_hangup() {
                            break;
                        }
                        if event.is_error() {
                            log::error!("an error occurs");
                            continue;
                        }

                        if event.is_invalid() {
                            self.sources.unregister(&event.key);
                            break;
                        }

                        if event.is_readable() {
                            let Some(mut stream) = self.conn.get(addr) else {
                                log::error!("connection not found `{addr}`");
                                continue;
                            };
                            let mut buff = String::new();
                            if let Err(err) = stream.read_to_string(&mut buff) {
                                if err.kind() != ErrorKind::WouldBlock {
                                    return Err(err);
                                }
                            }
                            let buff = buff.trim();

                            let requ: Request<Value> = serde_json::from_str(&buff).unwrap();
                            let callback = self.rpc_method.get(&requ.method).unwrap();

                            let resp = callback(&requ.params);
                            let resp = Response {
                                id: requ.id.clone().unwrap(),
                                jsonrpc: Some(requ.jsonrpc.clone()),
                                result: Some(resp),
                                error: None,
                            };
                            self.send_resp(addr.to_string(), resp);
                        }

                        if event.is_writable() {
                            let stream = self.conn.get(addr);
                            if stream.is_none() {
                                log::error!("connection not found `{addr}`");
                                continue;
                            };

                            let mut stream = stream.unwrap();
                            let Some(resp) = self.pop_resp(addr.to_string()) else {
                                break;
                            };
                            let buff = serde_json::to_string(&resp).unwrap();
                            if let Err(err) = stream.write_all(buff.as_bytes()) {
                                if err.kind() != ErrorKind::WouldBlock {
                                    return Err(err);
                                }
                            }
                            log::debug!("Response send `{buff}`");
                            match stream.flush() {
                                // In this case, we've written all the data, we
                                // are no longer interested in writing to this
                                // socket.
                                Ok(()) => {
                                    event.source.unset(popol::interest::WRITE);
                                }
                                // In this case, the write couldn't complete. Set
                                // our interest to `WRITE` to be notified when the
                                // socket is ready to write again.
                                Err(err)
                                    if [io::ErrorKind::WouldBlock, io::ErrorKind::WriteZero]
                                        .contains(&err.kind()) =>
                                {
                                    log::error!("blocking while flushing");
                                    event.source.set(popol::interest::WRITE);
                                }
                                Err(err) => {
                                    log::error!(target: "net", "{}: Write error: {}", addr, err.to_string());
                                }
                            }
                            stream.shutdown(std::net::Shutdown::Both)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop = true;
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
    use std::{io::Write, os::unix::net::UnixStream, path::Path, str::FromStr, time::Duration};

    use lampo_common::logger;
    use ntest::timeout;
    use serde_json::Value;

    use crate::{
        json_rpc2::{Request, Response, Id},
        JSONRPCv2,
    };

    #[test]
    #[timeout(9000)]
    fn register_rpc() {
        logger::init(log::Level::Debug).unwrap();
        let mut server = JSONRPCv2::new("/tmp/tmp.sock").unwrap();
        let _ = server.add_rpc("foo", |request| serde_json::json!(request));
        let res = server.add_rpc("secon", |request| serde_json::json!(request));
        assert!(res.is_ok(), "{:?}", res);

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

        let _ = worker.join();
    }
}
