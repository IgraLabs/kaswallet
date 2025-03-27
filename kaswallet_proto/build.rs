fn main() {
    tonic_build::configure()
        .out_dir("src")
        .compile_protos(&["proto/wallet.proto"], &["proto"])
        .unwrap_or_else(|e| panic!("Failed to compile proto file: {:?}", e));
}
