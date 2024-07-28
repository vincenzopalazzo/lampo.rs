# lampo-jsonrpc

## Description

Minimal and full feature pure async JSON RPC 2.0 server implementation.

## API Usage

Provide instructions on how to install your project. For example:

```rust
let path = "/tmp/tmp.sock";
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

// Client-side code
let client_worker = std::thread::spawn(move || {
    // [Add your client-side code here, similar to the example provided]
});
```

## Contributing

TODO

## License

This project is licensed under the GNU General Public License v2.0 only. For more information, see [LICENSE](https://www.gnu.org/licenses/old-licenses/gpl-2.0.txt).

