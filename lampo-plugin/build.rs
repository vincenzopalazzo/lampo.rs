fn main() {
    #[cfg(feature = "grpc")]
    {
        tonic_build::configure()
            .build_server(false) // daemon is the client
            .build_client(true)
            .compile_protos(&["proto/plugin.proto"], &["proto/"])
            .expect("failed to compile plugin.proto");
    }
}
