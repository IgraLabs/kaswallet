use clap::{Parser, ValueEnum};
use common::args::parse_network_type;
use common::error_location::ErrorLocation;
use common::errors::{UserInputError, WalletError, WalletResult};
use kaspa_consensus_core::network::NetworkId;
use kaspa_consensus_core::subnets::{SUBNETWORK_ID_NATIVE, SUBNETWORK_ID_SIZE, SubnetworkId};
use std::str::FromStr;
use tracing_subscriber::filter::LevelFilter as TracingLevelFilter;

/// Expected length of `--subnetwork-id` in hex characters: 20 bytes * 2.
const SUBNETWORK_ID_HEX_LEN: usize = SUBNETWORK_ID_SIZE * 2;

/// Validate and parse a `--subnetwork-id` hex string.
///
/// Used by clap's `value_parser` (parse-time validation) and by tests.
/// Rejects: wrong length, `0x` prefix, non-hex characters, reserved
/// built-in subnetworks (COINBASE/REGISTRY).
pub fn parse_subnetwork_id_arg(s: &str) -> Result<SubnetworkId, String> {
    if s.starts_with("0x") || s.starts_with("0X") {
        return Err("--subnetwork-id must not have a 0x prefix".to_string());
    }
    if s.len() != SUBNETWORK_ID_HEX_LEN {
        return Err(format!(
            "--subnetwork-id must be exactly {SUBNETWORK_ID_HEX_LEN} hex characters \
             ({SUBNETWORK_ID_SIZE} bytes), got {}",
            s.len(),
        ));
    }
    let parsed = SubnetworkId::from_str(s).map_err(|e| {
        format!(
            "--subnetwork-id must be {SUBNETWORK_ID_HEX_LEN} lowercase hex characters \
             ({SUBNETWORK_ID_SIZE} bytes), no 0x prefix: {e}"
        )
    })?;
    if parsed.is_builtin() {
        return Err(format!(
            "--subnetwork-id: reserved built-in subnetwork {parsed} is not allowed",
        ));
    }
    Ok(parsed)
}

/// Resolve `--subnetwork-id` into a concrete `SubnetworkId`,
/// defaulting to `SUBNETWORK_ID_NATIVE` when unset.
///
/// Kept as a thin wrapper so `Default::default()`-constructed `Args`
/// (used by tests) can still produce a usable value when the field is `None`.
pub fn resolve_subnetwork_id(arg: Option<SubnetworkId>) -> WalletResult<SubnetworkId> {
    Ok(arg.unwrap_or(SUBNETWORK_ID_NATIVE))
}

/// Clap adapter: parse + validate, returning `WalletError` on rejection.
///
/// Equivalent to `parse_subnetwork_id_arg` but wraps the error in our
/// `UserInputError` enum. Used by tests exercising the validation surface
/// without going through clap.
pub fn parse_subnetwork_id(arg: Option<&str>) -> WalletResult<SubnetworkId> {
    match arg {
        Some(s) => parse_subnetwork_id_arg(s).map_err(|reason| {
            WalletError::from(UserInputError::InvalidArgument {
                reason,
                location: ErrorLocation::capture(),
            })
        }),
        None => Ok(SUBNETWORK_ID_NATIVE),
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "kaswallet-daemon")]
pub struct Args {
    #[arg(long, help = "Use the test network")]
    pub testnet: bool,

    #[arg(long, default_value = "10", help = "Testnet network suffix number")]
    pub testnet_suffix: u32,

    #[arg(long, help = "Use the development test network")]
    pub devnet: bool,

    #[arg(long, help = "Use the simulation test network")]
    pub simnet: bool,

    // TODO: Remove when wallet is more stable
    #[arg(long = "enable-mainnet-pre-launch", hide = true)]
    pub enable_mainnet_pre_launch: bool,

    #[arg(long = "keys", short = 'k', help = "Path to keys file")]
    pub keys_file_path: Option<String>,

    #[arg(long, help = "Path to logs directory")]
    pub logs_path: Option<String>,

    #[arg(long, short = 'v', default_value = "info", help = "Log level")]
    pub logs_level: LogsLevel,

    #[arg(long, short = 's', help = "Kaspa node RPC server to connect to")]
    pub server: Option<String>,

    #[arg(
        long,
        short = 'l',
        default_value = "127.0.0.1:8082",
        help = "Address to listen on"
    )]
    pub listen: String,

    #[arg(
        long,
        env = "KASWALLET_SUBNETWORK_ID",
        value_parser = parse_subnetwork_id_arg,
        help = "Custom subnetwork ID (20 bytes / 40 lowercase hex characters, no 0x prefix). \
                Reserved built-in IDs are not permitted. Omit to use the native subnetwork. \
                Non-native subnetworks use transaction version 1."
    )]
    pub subnetwork_id: Option<SubnetworkId>,

    #[arg(long, help = "Enable tokio console")]
    #[cfg(debug_assertions)]
    pub enable_tokio_console: bool,

    #[arg(
        long,
        default_value = "10000",
        help = "Sync interval in milliseconds",
        hide = true
    )]
    pub sync_interval_millis: u64,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            testnet: false,
            testnet_suffix: 10,
            devnet: false,
            simnet: false,
            enable_mainnet_pre_launch: false,
            keys_file_path: None,
            logs_path: None,
            logs_level: Default::default(),
            server: None,
            listen: "".to_string(),
            subnetwork_id: None,
            #[cfg(debug_assertions)]
            enable_tokio_console: false,
            sync_interval_millis: 10,
        }
    }
}

#[derive(Debug, Clone, ValueEnum, Default)]
pub enum LogsLevel {
    Off,
    Trace,
    #[default]
    Debug,
    Info,
    Warn,
    Error,
}

impl From<&LogsLevel> for TracingLevelFilter {
    fn from(value: &LogsLevel) -> TracingLevelFilter {
        match value {
            LogsLevel::Off => TracingLevelFilter::OFF,
            LogsLevel::Trace => TracingLevelFilter::TRACE,
            LogsLevel::Debug => TracingLevelFilter::DEBUG,
            LogsLevel::Info => TracingLevelFilter::INFO,
            LogsLevel::Warn => TracingLevelFilter::WARN,
            LogsLevel::Error => TracingLevelFilter::ERROR,
        }
    }
}

impl Args {
    pub fn network_id(&self) -> NetworkId {
        parse_network_type(
            self.testnet,
            self.devnet,
            self.simnet,
            self.testnet_suffix,
            self.enable_mainnet_pre_launch,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const IGRA_LANE_ID_HEX: &str = "97b1000000000000000000000000000000000000";
    const SUBNETWORK_ID_COINBASE_HEX: &str = "0100000000000000000000000000000000000000";
    const SUBNETWORK_ID_REGISTRY_HEX: &str = "0200000000000000000000000000000000000000";
    const SUBNETWORK_ID_NATIVE_HEX: &str = "0000000000000000000000000000000000000000";

    #[test]
    fn args_subnetwork_id_flag_is_parsed() {
        let args = Args::try_parse_from(["kaswallet-daemon", "--subnetwork-id", IGRA_LANE_ID_HEX])
            .expect("clap should parse a valid subnetwork-id flag");
        let parsed = args
            .subnetwork_id
            .expect("subnetwork_id must be Some when flag is provided");
        let bytes: &[u8] = parsed.as_ref();
        assert_eq!(bytes[0], 0x97);
        assert_eq!(bytes[1], 0xb1);
    }

    #[test]
    fn args_subnetwork_id_default_is_none() {
        let args =
            Args::try_parse_from(["kaswallet-daemon"]).expect("clap should parse with no flags");
        assert!(args.subnetwork_id.is_none(), "default must be None");
    }

    #[test]
    fn args_clap_rejects_malformed_subnetwork_id() {
        // Parse-time validation: bad hex never produces an Args struct.
        let err = Args::try_parse_from([
            "kaswallet-daemon",
            "--subnetwork-id",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ])
        .expect_err("malformed hex must be rejected at parse time");
        let msg = err.to_string();
        assert!(
            msg.contains("subnetwork-id") || msg.contains("hex"),
            "error must mention the offending flag/value, got: {msg}"
        );
    }

    #[rstest]
    #[case::native(SUBNETWORK_ID_NATIVE_HEX)]
    #[case::igra_lane(IGRA_LANE_ID_HEX)]
    fn parse_subnetwork_id_arg_accepts_valid_ids(#[case] hex: &str) {
        parse_subnetwork_id_arg(hex)
            .unwrap_or_else(|e| panic!("expected {hex} to parse, got: {e}"));
    }

    #[rstest]
    #[case::empty("")]
    #[case::short("97b10000")]
    #[case::one_too_short("97b100000000000000000000000000000000000")] // 39
    #[case::one_too_long("97b10000000000000000000000000000000000000")] // 41
    #[case::eighty(
        "97b1000000000000000000000000000000000000ffffffffffffffffffffffffffffffffffffffff"
    )] // 80
    fn parse_subnetwork_id_arg_rejects_wrong_length(#[case] hex: &str) {
        let err = parse_subnetwork_id_arg(hex).expect_err("wrong-length input must be rejected");
        assert!(
            err.contains("hex characters") || err.contains("0x prefix"),
            "error must explain length requirement, got: {err}"
        );
    }

    #[rstest]
    #[case::zero_x_prefix_lower("0xb1000000000000000000000000000000000000000")]
    #[case::zero_x_prefix_upper("0Xb1000000000000000000000000000000000000000")]
    fn parse_subnetwork_id_arg_rejects_0x_prefix(#[case] hex: &str) {
        let err = parse_subnetwork_id_arg(hex).expect_err("0x prefix must be rejected");
        assert!(
            err.contains("0x prefix"),
            "error must mention 0x prefix, got: {err}"
        );
    }

    #[test]
    fn parse_subnetwork_id_arg_rejects_non_hex_chars() {
        let err = parse_subnetwork_id_arg("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")
            .expect_err("non-hex characters must be rejected");
        assert!(
            err.contains("hex"),
            "error must mention hex format, got: {err}"
        );
    }

    #[rstest]
    #[case::coinbase(SUBNETWORK_ID_COINBASE_HEX)]
    #[case::registry(SUBNETWORK_ID_REGISTRY_HEX)]
    fn parse_subnetwork_id_arg_rejects_builtin_ids(#[case] hex: &str) {
        let err = parse_subnetwork_id_arg(hex)
            .expect_err("reserved built-in subnetwork ids must be rejected");
        assert!(
            err.contains("built-in"),
            "error must mention built-in rejection, got: {err}"
        );
    }

    #[test]
    fn parse_subnetwork_id_returns_native_when_unset() {
        let parsed = parse_subnetwork_id(None).expect("None should resolve to NATIVE");
        assert_eq!(parsed, SUBNETWORK_ID_NATIVE);
    }

    #[test]
    fn parse_subnetwork_id_explicit_native_round_trips_to_native() {
        let parsed = parse_subnetwork_id(Some(SUBNETWORK_ID_NATIVE_HEX))
            .expect("explicit all-zeros must parse");
        assert_eq!(parsed, SUBNETWORK_ID_NATIVE);
        assert!(parsed.is_native());
    }

    #[test]
    fn parse_subnetwork_id_wraps_arg_error_in_user_input_error() {
        let err = parse_subnetwork_id(Some("97b10000"))
            .expect_err("anything shorter than 40 chars must be rejected");
        assert!(
            matches!(
                err,
                WalletError::UserInput(UserInputError::InvalidArgument { .. })
            ),
            "expected InvalidArgument, got: {err}"
        );
    }

    #[test]
    fn resolve_subnetwork_id_returns_native_for_none() {
        let parsed = resolve_subnetwork_id(None).expect("None should resolve to NATIVE");
        assert_eq!(parsed, SUBNETWORK_ID_NATIVE);
    }

    #[test]
    fn resolve_subnetwork_id_returns_provided_value() {
        let id = parse_subnetwork_id_arg(IGRA_LANE_ID_HEX).expect("valid id");
        let parsed = resolve_subnetwork_id(Some(id.clone())).expect("Some should pass through");
        assert_eq!(parsed, id);
    }
}
