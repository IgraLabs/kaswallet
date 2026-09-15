// tonic generates every service method as `Result<Response<T>, tonic::Status>`, and `Status` is
// 176 bytes, which trips `clippy::result_large_err` on newer toolchains. The offending code is
// generated into OUT_DIR by `tonic::include_proto!`, so the lint cannot be acted on here and the
// suggested remedy (boxing) would have to come from tonic itself.
#[allow(clippy::result_large_err)]
pub mod kaswallet_proto {
    tonic::include_proto!("kaswallet_proto");
}
