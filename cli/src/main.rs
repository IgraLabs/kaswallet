use clap::{Parser, Subcommand};
use std::process;

mod commands;
mod utils;

const DEFAULT_DAEMON_ADDRESS: &str = "http://127.0.0.1:8082";

#[derive(Parser)]
#[command(name = "kaswallet-cli")]
#[command(about = "Kaspa wallet CLI client", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Shows the balance of the wallet
    Balance {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,

        /// Show balance per address
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Shows all generated public addresses of the current wallet
    ShowAddresses {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,
    },

    /// Generates a new public address of the current wallet
    NewAddress {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,
    },

    /// Get the wallet daemon version
    GetDaemonVersion {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,
    },

    /// Get UTXOs for the wallet
    GetUtxos {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,

        /// Specific addresses to get UTXOs for (can be specified multiple times)
        #[arg(short = 'a', long = "address")]
        addresses: Vec<String>,

        /// Include pending coinbase UTXOs
        #[arg(long = "include-pending")]
        include_pending: bool,

        /// Include dust UTXOs (UTXOs whose value is less than the fee to spend them)
        #[arg(long = "include-dust")]
        include_dust: bool,
    },

    /// Sends a Kaspa transaction to a public address
    Send {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,

        /// The public address to send Kaspa to
        #[arg(short = 't', long = "to-address")]
        to_address: String,

        /// An amount to send in Kaspa (e.g. 1234.12345678)
        #[arg(short = 'v', long = "send-amount", conflicts_with = "send_all")]
        send_amount: Option<String>,

        /// Send all the Kaspa in the wallet
        #[arg(long = "send-all", conflicts_with = "send_amount")]
        send_all: bool,

        /// Specific public address to send Kaspa from (can be specified multiple times)
        #[arg(short = 'a', long = "from-address")]
        from_addresses: Vec<String>,

        /// Use an existing change address instead of generating a new one
        #[arg(short = 'u', long = "use-existing-change-address")]
        use_existing_change_address: bool,

        /// Maximum fee rate in Sompi/gram
        #[arg(short = 'm', long = "max-fee-rate", conflicts_with_all = ["fee_rate", "max_fee"])]
        max_fee_rate: Option<f64>,

        /// Exact fee rate in Sompi/gram
        #[arg(short = 'r', long = "fee-rate", conflicts_with_all = ["max_fee_rate", "max_fee"])]
        fee_rate: Option<f64>,

        /// Maximum fee in Sompi
        #[arg(short = 'x', long = "max-fee", conflicts_with_all = ["max_fee_rate", "fee_rate"])]
        max_fee: Option<u64>,

        /// Wallet password
        #[arg(short = 'p', long = "password")]
        password: Option<String>,

        /// Show serialized transactions
        #[arg(short = 's', long = "show-serialized")]
        show_serialized: bool,
    },

    /// Create an unsigned Kaspa transaction
    CreateUnsignedTransaction {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,

        /// The public address to send Kaspa to
        #[arg(short = 't', long = "to-address")]
        to_address: String,

        /// An amount to send in Kaspa (e.g. 1234.12345678)
        #[arg(short = 'v', long = "send-amount", conflicts_with = "send_all")]
        send_amount: Option<String>,

        /// Send all the Kaspa in the wallet
        #[arg(long = "send-all", conflicts_with = "send_amount")]
        send_all: bool,

        /// Specific public address to send Kaspa from (can be specified multiple times)
        #[arg(short = 'a', long = "from-address")]
        from_addresses: Vec<String>,

        /// Use an existing change address instead of generating a new one
        #[arg(short = 'u', long = "use-existing-change-address")]
        use_existing_change_address: bool,

        /// Maximum fee rate in Sompi/gram
        #[arg(short = 'm', long = "max-fee-rate", conflicts_with_all = ["fee_rate", "max_fee"])]
        max_fee_rate: Option<f64>,

        /// Exact fee rate in Sompi/gram
        #[arg(short = 'r', long = "fee-rate", conflicts_with_all = ["max_fee_rate", "max_fee"])]
        fee_rate: Option<f64>,

        /// Maximum fee in Sompi
        #[arg(short = 'x', long = "max-fee", conflicts_with_all = ["max_fee_rate", "fee_rate"])]
        max_fee: Option<u64>,
    },

    /// Sign the given unsigned transaction(s)
    Sign {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,

        /// The unsigned transaction(s) to sign (encoded in hex)
        #[arg(short = 't', long = "transaction", conflicts_with = "transaction_file")]
        transaction: Option<String>,

        /// File containing the unsigned transaction(s) to sign (encoded in hex)
        #[arg(short = 'F', long = "transaction-file", conflicts_with = "transaction")]
        transaction_file: Option<String>,

        /// Wallet password
        #[arg(short = 'p', long = "password")]
        password: Option<String>,
    },

    /// Broadcast the given signed transaction(s)
    Broadcast {
        #[arg(short = 'd', long = "daemonaddress", default_value = DEFAULT_DAEMON_ADDRESS)]
        daemon_address: String,

        /// The signed transaction(s) to broadcast (encoded in hex)
        #[arg(short = 't', long = "transaction", conflicts_with = "transaction_file")]
        transaction: Option<String>,

        /// File containing the signed transaction(s) to broadcast (encoded in hex)
        #[arg(short = 'F', long = "transaction-file", conflicts_with = "transaction")]
        transaction_file: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Balance {
            daemon_address,
            verbose,
        } => commands::balance(&daemon_address, verbose).await,

        Commands::ShowAddresses { daemon_address } => {
            commands::show_addresses(&daemon_address).await
        }

        Commands::NewAddress { daemon_address } => commands::new_address(&daemon_address).await,

        Commands::GetDaemonVersion { daemon_address } => {
            commands::get_daemon_version(&daemon_address).await
        }

        Commands::GetUtxos {
            daemon_address,
            addresses,
            include_pending,
            include_dust,
        } => commands::get_utxos(&daemon_address, addresses, include_pending, include_dust).await,

        Commands::Send {
            daemon_address,
            to_address,
            send_amount,
            send_all,
            from_addresses,
            use_existing_change_address,
            max_fee_rate,
            fee_rate,
            max_fee,
            password,
            show_serialized,
        } => {
            commands::send(
                &daemon_address,
                &to_address,
                send_amount.as_deref(),
                send_all,
                from_addresses,
                use_existing_change_address,
                max_fee_rate,
                fee_rate,
                max_fee,
                password,
                show_serialized,
            )
            .await
        }

        Commands::CreateUnsignedTransaction {
            daemon_address,
            to_address,
            send_amount,
            send_all,
            from_addresses,
            use_existing_change_address,
            max_fee_rate,
            fee_rate,
            max_fee,
        } => {
            commands::create_unsigned_transaction(
                &daemon_address,
                &to_address,
                send_amount.as_deref(),
                send_all,
                from_addresses,
                use_existing_change_address,
                max_fee_rate,
                fee_rate,
                max_fee,
            )
            .await
        }

        Commands::Sign {
            daemon_address,
            transaction,
            transaction_file,
            password,
        } => commands::sign(&daemon_address, transaction, transaction_file, password).await,

        Commands::Broadcast {
            daemon_address,
            transaction,
            transaction_file,
        } => commands::broadcast(&daemon_address, transaction, transaction_file).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
