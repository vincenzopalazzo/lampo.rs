fn main() {
    #[cfg(feature = "grpc")]
    {
        // prost-build shells out to `protoc`. Point it at the vendored binary
        // so the grpc feature builds without a system protobuf-compiler.
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());
        tonic_build::configure()
            .build_server(false) // daemon is the client
            .build_client(true)
            .compile_protos(&["proto/plugin.proto"], &["proto/"])
            .expect("failed to compile plugin.proto");
    }
}
