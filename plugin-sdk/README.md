# plugin-sdk

Language SDKs for the lampo plugin protocol (JSON-RPC 2.0 over stdin/stdout,
optional gRPC). The daemon does not care which SDK built the binary.

| Directory | Language | Transports |
| --- | --- | --- |
| [`rust/`](rust/) | Rust | stdio and gRPC (`--lampo-listen`) |
| [`zig/`](zig/) | Zig 0.17.0-dev.1476 | stdio and gRPC (`--lampo-listen`, Zrpc) |

`lampo-bitcoind/` is the Rust chain backend. `lampo-bitcoind/zig/` is the
same plugin in Zig: same RPC methods, same `init` options (`core-url`,
`core-user`, `core-pass`), same `important` flag.

```sh
# Rust (workspace member). Zig 0.16 is not enough: Zrpc needs the pin in
# plugin-sdk/zig/.zigversion.
cargo build -p lampo-bitcoind -p lampo-plugin-sdk

# Zig
cd plugin-sdk/zig && zig build test
cd lampo-bitcoind/zig && zig build -Doptimize=ReleaseSafe
```

The Zig toolchain is `0.17.0-dev.1476+91a29d707`. The official tarball for
that build is gone; the Mach mirror still has it:

```sh
curl -L -o zig.tar.xz https://pkg.hexops.org/zig/zig-aarch64-macos-0.17.0-dev.1476+91a29d707.tar.xz
```

`lampod-cli` starts a binary named `lampo-bitcoind` that sits next to it.
Either build can fill that slot.

## rbitcoin

[rbitcoin](https://github.com/reardencode/rbitcoin) serves the same Core
JSON-RPC methods this plugin forwards (`getblockchaininfo`, `getblock`,
`getblockheader`, `estimatesmartfee`, `getmempoolinfo`,
`sendrawtransaction`). Point `core-url` at its `--rpc-listen` address.
No second plugin.

The plugin sends HTTP Basic from `core-user` and `core-pass`. rbitcoin
accepts that when `--rpc-cookie-file` is a `username:password` file (it
does not create the file). The bearer token in `rpc.token` and the unix
socket are a different login. This plugin does not speak them.

```sh
rbitcoin-node --datadir ./datadir-signet --network signet \
  --rpc-listen 127.0.0.1:38332 \
  --rpc-cookie-file ./datadir-signet/rpc.cookie
```

```
backend=core
core-url=http://127.0.0.1:38332
core-user=<user from the cookie>
core-pass=<password from the cookie>
```

The on-chain wallet uses that same URL and user/password. Its block scan
is `getblock` / `getblockhash`, so `--sh-index` is not required.

`estimatesmartfee` comes back in Core's JSON. The number is rbitcoin's
10-minute feerate. Lampo calls `sendrawtransaction` with only the raw
hex, so rbitcoin's default fee cap applies (10000 sat/vB).
