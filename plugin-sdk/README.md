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
