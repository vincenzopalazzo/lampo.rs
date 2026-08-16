use std::env;
use std::io::Result;
use std::path::PathBuf;

fn main() -> Result<()> {
    // Prefer a system protoc when present; fall back to the vendored binary so
    // CI machines without protobuf-compiler still build.
    if env::var_os("PROTOC").is_none() {
        if let Ok(protoc) = protoc_bin_vendored::protoc_bin_path() {
            // SAFETY: build-script exclusive; no concurrent readers of PROTOC.
            unsafe { env::set_var("PROTOC", protoc) };
        }
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let descriptor_path = out_dir.join("proto_descriptor.bin");

    let mut config = prost_build::Config::new();
    config
        .file_descriptor_set_path(&descriptor_path)
        .compile_well_known_types()
        .extern_path(".google.protobuf", "::pbjson_types")
        .bytes(["."]);

    config.compile_protos(&["proto/lightning.proto"], &["proto/"])?;

    let descriptor_set = std::fs::read(&descriptor_path)?;
    pbjson_build::Builder::new()
        .register_descriptors(&descriptor_set)?
        .build(&[".lnrpc"])?;

    Ok(())
}
