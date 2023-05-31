//! Exposing C FFI for interact with Lampo API
//! and build easly a node.
use std::cell::Cell;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Once;

use libc;

pub use lampod::LampoDeamon;

#[macro_export]
macro_rules! null {
    () => {
        std::ptr::null_mut()
    };
}

#[macro_export]
macro_rules! from_cstr {
    ($x:expr) => {{
        use std::ffi::CStr;
        let c_str = unsafe { CStr::from_ptr($x) };
        let c_str = c_str.to_str();
        if c_str.is_err() {
            None
        } else {
            Some(c_str.unwrap())
        }
    }};
}

#[macro_export]
macro_rules! to_cstr {
    ($x:expr) => {{
        use std::ffi::CString;
        let Ok(c_str) = CString::new($x) else {
                                                                                    return null!()
                                                                                };
        c_str.into_raw()
    }};
}

#[macro_export]
macro_rules! json_buffer {
    ($x:expr) => {{
        use lampo_common::json;
        let Ok(buff) = json::to_string_pretty($x) else { return null!() };
        to_cstr!(buff)
    }};
}

#[macro_export]
macro_rules! c_free {
    ($x:expr) => {{
        if !$x.is_null() {
            unsafe { Box::from_raw($x) };
        }
    }};
}

#[macro_export]
macro_rules! as_rust {
    ($x:expr) => {{
        if !$x.is_null() {
            unsafe { Some(Arc::from_raw($x)) }
        } else {
            None
        }
    }};
}

static INIT: Once = Once::new();

static LAST_ERR: Mutex<Cell<Option<String>>> = Mutex::new(Cell::new(None));

fn init_logger() {
    // ignore error
    INIT.call_once(|| {
        use lampo_common::logger;
        logger::init(logger::Level::Debug).expect("Unable to init the logger");
    });
}

/// Allow to create a lampo deamon from a configuration patch!
#[no_mangle]
#[allow(unused_variables)]
#[allow(unused_assignments)]
pub extern "C" fn new_lampod(conf_path: *const libc::c_char) -> *mut LampoDeamon {
    use lampo_common::bitcoin;
    use lampo_common::conf::LampoConf;
    use lampo_common::secp256k1;
    use lampo_nakamoto::{Config, Nakamoto, Network};
    use lampod::chain::{LampoWalletManager, WalletManager};
    use std::str::FromStr;

    init_logger();

    let conf_path_t = from_cstr!(conf_path);
    if conf_path_t.is_none() {
        LAST_ERR
            .lock()
            .unwrap()
            .set(Some(format!("error: invalid c string `{:?}`", conf_path)));
        return null!();
    }
    let conf = match LampoConf::try_from(conf_path_t.unwrap().to_owned()) {
        Ok(conf) => conf,
        // FIXME: log the error!
        Err(err) => {
            LAST_ERR
                .lock()
                .unwrap()
                .set(Some(format!("error reading conf {:?}", err)));
            return null!();
        }
    };

    let wallet = if let Some(ref priv_key) = conf.private_key {
        #[cfg(not(debug_assertions))]
        compile_error!(
            "this should not be allowed, will be not possible set custom keys in release build!"
        );
        let Ok(key) = secp256k1::SecretKey::from_str(&priv_key) else {
            LAST_ERR.lock().unwrap().set(Some(format!("invalid private key `{priv_key}`")));
            return null!();
        };
        let key = bitcoin::PrivateKey::new(key, conf.network);
        let Ok(wallet) = LampoWalletManager::try_from((key, conf.channels_keys.clone())) else {
            LAST_ERR.lock().unwrap().set(Some(format!("error init wallet")));
            return null!();
        };
        wallet
    } else {
        let Ok(wallet) = LampoWalletManager::new(conf.network) else {
            LAST_ERR.lock().unwrap().set(Some(format!("error init wallet")));
            return null!();
        };
        wallet
    };

    let mut nakamtot_conf = Config::default();
    nakamtot_conf.network = Network::from_str(&conf.network.to_string()).unwrap();
    let client = Arc::new(Nakamoto::new(nakamtot_conf).unwrap());
    let mut lampod = LampoDeamon::new(conf, Arc::new(wallet));
    if let Err(err) = lampod.init(client) {
        LAST_ERR
            .lock()
            .unwrap()
            .set(Some(format!("error while init the node {:?}", err)));
        return null!();
    }
    let lampod = Box::new(lampod);
    Box::into_raw(lampod)
}

/// Add a JSON RPC 2.0 Sever that listen on a unixsocket, and return a error code
/// < 0 is an error happens, or 0 is all goes well.
#[no_mangle]
pub extern "C" fn lampo_last_errror() -> *const libc::c_char {
    let value = LAST_ERR.lock().unwrap().take();
    if let Some(value) = value {
        return to_cstr!(value);
    }
    null!()
}
/// Add a JSON RPC 2.0 Sever that listen on a unixsocket, and return a error code
/// < 0 is an error happens, or 0 is all goes well.
#[no_mangle]
pub extern "C" fn add_jsonrpc_on_unixsocket(lampod: *mut LampoDeamon) -> i64 {
    use lampo_jsonrpc::JSONRPCv2;
    use lampod::jsonrpc::inventory::get_info;
    use lampod::jsonrpc::open_channel::json_open_channel;
    use lampod::jsonrpc::peer_control::json_connect;
    use lampod::jsonrpc::CommandHandler;

    let Some(lampod) = as_rust!(lampod) else {
        return -1;
    };
    let socket_path = format!("{}/lampod.socket", lampod.root_path());
    std::env::set_var("LAMPO_UNIX", socket_path.clone());
    let Ok(server) = JSONRPCv2::new(lampod.clone(), &socket_path) else {
        return -2;
    };
    server.add_rpc("getinfo", get_info).unwrap();
    server.add_rpc("connect", json_connect).unwrap();
    server.add_rpc("fundchannel", json_open_channel).unwrap();
    let rpc_handler = server.handler();
    let Ok(lampo_handler) = CommandHandler::new(lampod.conf()) else {
        return -2;
    };
    lampo_handler.set_handler(rpc_handler);
    let lampo_handler = Arc::new(lampo_handler);
    let Ok(()) = lampod.add_external_handler(lampo_handler.clone()) else {
        return -2;
    };

    // FIXME: this is blocking?
    let _ = server.spawn().join();
    0
}

#[no_mangle]
pub extern "C" fn lampod_call(
    lampod: *mut LampoDeamon,
    method: *const libc::c_char,
    buffer: *const libc::c_char,
) -> *const libc::c_char {
    use lampo_common::json;

    let Some(lampod) = as_rust!(lampod) else {
        return null!();
    };
    let method = from_cstr!(method);
    let buffer = from_cstr!(buffer);
    // FIXME: check for error here before unwrap
    let Ok(payload) = json::from_str::<json::Value>(buffer.unwrap()) else {
        return null!();
    };
    // FIXME: check for error before unwrap
    let response = lampod.call(method.unwrap(), payload);
    // FIXME Encode this to a string
    match response {
        Ok(resp) => json_buffer!(&resp),
        Err(_) => null!(),
    }
}

/// Allow to create a lampo deamon from a configuration patch!
#[no_mangle]
pub extern "C" fn lampo_listen(lampod: *mut LampoDeamon) {
    let Some(lampod) = as_rust!(lampod) else {
        panic!("errr during the convertion");
    };
    // this will start the lampod in background, without
    // impact on the binding language
    std::thread::spawn(move || lampod.listen().map(|lampod| lampod.join()));
}

/// Allow to create a lampo deamon from a configuration patch!
#[no_mangle]
pub extern "C" fn free_lampod(lampod: *mut LampoDeamon) {
    c_free!(lampod);
}
