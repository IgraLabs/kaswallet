use clap::{Parser, ValueEnum};
use common::args::parse_network_type;
use kaspa_consensus_core::network::NetworkId;
use kaspa_consensus_core::subnets::{SUBNETWORK_ID_NATIVE, SUBNETWORK_NAMESPACE_LEN, SubnetworkId};
use tracing_subscriber::filter::LevelFilter as TracingLevelFilter;

/// Hex length of the 4-byte user-lane namespace (8 chars).
const SUBNETWORK_NAMESPACE_HEX_LEN: usize = SUBNETWORK_NAMESPACE_LEN * 2;

/// Validate and parse a `--subnetwork-id` hex string.
///
/// Accepts the 4-byte user-lane namespace as 8 lowercase hex chars (e.g.
/// `97b10000`) and zero-pads it to the full 20-byte SubnetworkId per the
/// KIP-21 `[namespace, 0×16]` shape. Rejects 0x prefix, wrong length,
/// non-hex chars, reserved built-in subnetworks, and the reserved-system
/// shape `[x, 0×19]` for `x` other than NATIVE.
pub fn parse_subnetwork_id_arg(s: &str) -> Result<SubnetworkId, String> {
    if s.starts_with("0x") || s.starts_with("0X") {
        return Err("--subnetwork-id must not have a 0x prefix".to_string());
    }
    if s.len() != SUBNETWORK_NAMESPACE_HEX_LEN {
        return Err(format!(
            "--subnetwork-id must be the 4-byte lane namespace as \
             {SUBNETWORK_NAMESPACE_HEX_LEN} lowercase hex chars (e.g. 97b10000), got {}",
            s.len(),
        ));
    }
    // Enforce lowercase explicitly. `SubnetworkId::from_str` / faster_hex
    // accept mixed/uppercase silently, which would let `97B10000` parse to
    // the same lane as `97b10000` — operationally confusing for grep/log
    // tooling that compares against the canonical lowercase form.
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(
            "--subnetwork-id must be lowercase hex (got mixed/uppercase characters)".to_string(),
        );
    }
    // Decode the 4-byte namespace directly into a fixed buffer and rely on
    // `SubnetworkId::from_namespace` to apply the KIP-21 zero-tail shape,
    // rather than allocating a 40-char padded `String` and re-decoding.
    let mut namespace = [0u8; SUBNETWORK_NAMESPACE_LEN];
    hex::decode_to_slice(s, &mut namespace)
        .map_err(|e| format!("--subnetwork-id namespace must be valid lowercase hex: {e}"))?;
    let parsed = SubnetworkId::from_namespace(namespace);
    if parsed.is_builtin() {
        return Err(format!(
            "--subnetwork-id: reserved built-in subnetwork {parsed} is not allowed",
        ));
    }
    // The reserved-system shape `[x, 0×19]` (i.e. a namespace where bytes
    // 1..4 are all zero) is rejected by consensus as `SubnetworksDisabled`
    // for any `x` other than NATIVE/COINBASE. Catch it at parse time so
    // operators see a clearer error than a mempool rejection. NATIVE is
    // allowed — passing `00000000` is equivalent to omitting the flag.
    if !parsed.is_native()
        && namespace[1..SUBNETWORK_NAMESPACE_LEN]
            .iter()
            .all(|&b| b == 0)
    {
        return Err(format!(
            "--subnetwork-id: namespace 0x{:02x}000000 collides with the reserved system \
             shape [x, 0×19] per KIP-21",
            namespace[0],
        ));
    }
    Ok(parsed)
}

/// Resolve `--subnetwork-id` into a concrete `SubnetworkId`,
/// defaulting to `SUBNETWORK_ID_NATIVE` when unset.
///
/// Kept as a thin wrapper so `Default::default()`-constructed `Args`
/// (used by tests) can still produce a usable value when the field is `None`.
pub fn resolve_subnetwork_id(arg: Option<SubnetworkId>) -> SubnetworkId {
    arg.unwrap_or(SUBNETWORK_ID_NATIVE)
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
        help = "Custom subnetwork ID as the 4-byte lane namespace (8 lowercase \
                hex chars, e.g. 97b10000). No 0x prefix. The wallet zero-pads it \
                to the full 20-byte on-chain id per KIP-21. Reserved built-in \
                IDs are not permitted. Omit to use the native subnetwork. \
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

    const IGRA_LANE_NAMESPACE_HEX: &str = "97b10000";
    const COINBASE_NAMESPACE_HEX: &str = "01000000";
    const REGISTRY_NAMESPACE_HEX: &str = "02000000";
    const NATIVE_NAMESPACE_HEX: &str = "00000000";

    #[test]
    fn args_subnetwork_id_flag_is_parsed() {
        let args = Args::try_parse_from([
            "kaswallet-daemon",
            "--subnetwork-id",
            IGRA_LANE_NAMESPACE_HEX,
        ])
        .expect("clap should parse a valid subnetwork-id flag");
        let parsed = args
            .subnetwork_id
            .expect("subnetwork_id must be Some when flag is provided");
        let bytes: &[u8] = parsed.as_ref();
        assert_eq!(bytes[0], 0x97);
        assert_eq!(bytes[1], 0xb1);
        // The wallet must have zero-padded the rest of the on-chain id.
        assert!(bytes[4..].iter().all(|&b| b == 0));
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
        let err = Args::try_parse_from(["kaswallet-daemon", "--subnetwork-id", "zzzzzzzz"])
            .expect_err("malformed hex must be rejected at parse time");
        let msg = err.to_string();
        assert!(
            msg.contains("subnetwork-id") || msg.contains("hex"),
            "error must mention the offending flag/value, got: {msg}"
        );
    }

    #[rstest]
    #[case::native(NATIVE_NAMESPACE_HEX)]
    #[case::igra_lane(IGRA_LANE_NAMESPACE_HEX)]
    fn parse_subnetwork_id_arg_accepts_valid_namespaces(#[case] hex: &str) {
        parse_subnetwork_id_arg(hex)
            .unwrap_or_else(|e| panic!("expected {hex} to parse, got: {e}"));
    }

    #[test]
    fn parse_subnetwork_id_arg_zero_pads_namespace_to_full_id() {
        let parsed = parse_subnetwork_id_arg(IGRA_LANE_NAMESPACE_HEX).expect("valid namespace");
        let bytes: &[u8] = parsed.as_ref();
        assert_eq!(&bytes[..SUBNETWORK_NAMESPACE_LEN], [0x97, 0xb1, 0x00, 0x00]);
        assert!(
            bytes[SUBNETWORK_NAMESPACE_LEN..].iter().all(|&b| b == 0),
            "trailing bytes must be zero-padded"
        );
    }

    #[rstest]
    #[case::empty("")]
    #[case::one_too_short("97b1000")] // 7
    #[case::one_too_long("97b100000")] // 9
    #[case::full_id_no_longer_accepted("97b1000000000000000000000000000000000000")] // 40
    fn parse_subnetwork_id_arg_rejects_wrong_length(#[case] hex: &str) {
        let err = parse_subnetwork_id_arg(hex).expect_err("wrong-length input must be rejected");
        assert!(
            err.contains("hex chars") || err.contains("0x prefix"),
            "error must explain length requirement, got: {err}"
        );
    }

    #[rstest]
    #[case::single_byte_namespace("97000000")]
    fn parse_subnetwork_id_arg_rejects_reserved_system_shape(#[case] hex: &str) {
        let err = parse_subnetwork_id_arg(hex)
            .expect_err("reserved system shape [x, 0×19] must be rejected");
        assert!(
            err.contains("reserved system shape"),
            "error must mention reserved system shape, got: {err}"
        );
    }

    #[rstest]
    #[case::zero_x_prefix_lower("0x97b100")]
    #[case::zero_x_prefix_upper("0X97b100")]
    fn parse_subnetwork_id_arg_rejects_0x_prefix(#[case] hex: &str) {
        let err = parse_subnetwork_id_arg(hex).expect_err("0x prefix must be rejected");
        assert!(
            err.contains("0x prefix"),
            "error must mention 0x prefix, got: {err}"
        );
    }

    #[test]
    fn parse_subnetwork_id_arg_rejects_non_hex_chars() {
        let err =
            parse_subnetwork_id_arg("zzzzzzzz").expect_err("non-hex characters must be rejected");
        assert!(
            err.contains("hex"),
            "error must mention hex format, got: {err}"
        );
    }

    #[rstest]
    #[case::coinbase(COINBASE_NAMESPACE_HEX)]
    #[case::registry(REGISTRY_NAMESPACE_HEX)]
    fn parse_subnetwork_id_arg_rejects_builtin_ids(#[case] hex: &str) {
        let err = parse_subnetwork_id_arg(hex)
            .expect_err("reserved built-in subnetwork ids must be rejected");
        assert!(
            err.contains("built-in"),
            "error must mention built-in rejection, got: {err}"
        );
    }

    #[test]
    fn parse_subnetwork_id_arg_explicit_native_namespace_round_trips_to_native() {
        let parsed =
            parse_subnetwork_id_arg(NATIVE_NAMESPACE_HEX).expect("explicit all-zeros must parse");
        assert_eq!(parsed, SUBNETWORK_ID_NATIVE);
        assert!(parsed.is_native());
    }

    #[test]
    fn resolve_subnetwork_id_returns_native_for_none() {
        let parsed = resolve_subnetwork_id(None);
        assert_eq!(parsed, SUBNETWORK_ID_NATIVE);
    }

    #[test]
    fn resolve_subnetwork_id_returns_provided_value() {
        let id = parse_subnetwork_id_arg(IGRA_LANE_NAMESPACE_HEX).expect("valid namespace");
        let parsed = resolve_subnetwork_id(Some(id));
        assert_eq!(parsed, id);
    }
}
