use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let protos = [
        "lightning.proto",
        "invoicesrpc/invoices.proto",
        "lndkrpc/lndkrpc.proto",
        "routerrpc/router.proto",
        "signrpc/signer.proto",
        "walletrpc/walletkit.proto",
    ];

    let dir = PathBuf::from("lnrpc");

    let proto_paths: Vec<_> = protos.iter().map(|proto| dir.join(proto)).collect();

    tonic_build::configure()
        .file_descriptor_set_path(out_dir.join("descriptor.bin"))
        .build_client(false)
        .build_server(true)
        .compile(&proto_paths, &[dir])?;

    Ok(())
}