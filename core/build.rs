fn main() {
    tonic_build::compile_protos("proto/wallet.proto")
        .unwrap_or_else(|e| panic!("Failed to compile proto file: {:?}", e));
}
