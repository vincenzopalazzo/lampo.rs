// BOLT 1 peer backup is not a Cargo feature. LDK only encrypts and queues our
// channel-monitor blob when `cfg(peer_storage)` is set.
//
// This lives in build.rs, not a `compile_error!` in the crate. `cargo test`
// recompiles the crate with rustdoc, and rustdoc does not read
// `.cargo/config.toml` rustflags, so an in-crate guard fails every doctest
// run even when the library itself was built correctly.
fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_PEER_STORAGE");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-changed=.cargo/config.toml");
    println!("cargo:rerun-if-changed=../.cargo/config.toml");

    if !cfg!(peer_storage) {
        panic!(
            "peer backup requires `--cfg peer_storage` (.cargo/config.toml). \
             RUSTFLAGS and CARGO_ENCODED_RUSTFLAGS replace that file, so append \
             the cfg there too. rustdoc does not read .cargo/config.toml; do not \
             move this check into the crate."
        );
    }
}
