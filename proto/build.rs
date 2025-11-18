fn main() {
    let proto_files = &["src/wallet.proto"];
    let dirs = &["src"];

    tonic_build::configure()
        .bytes([".kaswallet_proto"])
        .build_server(true)
        .build_client(true)
        .compile_protos(&proto_files[0..1], dirs)
        .unwrap_or_else(|e| panic!("protobuf compilation failed, error: {e}"));

    // recompile protobufs only if any of the proto files changes.
    for file in proto_files {
        println!("cargo:rerun-if-changed={file}");
    }
}
