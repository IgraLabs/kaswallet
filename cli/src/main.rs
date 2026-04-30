use args::{Args, Commands};
use clap::Parser;
use common::errors::{ErrorCategory, WalletError};
use std::process;

mod args;
mod commands;
mod utils;

// Process exit codes loosely modelled on `sysexits.h` so shells / CI scripts
// can branch on the *kind* of failure without parsing stderr. The mapping is
// exhaustive — adding a new `ErrorCategory` variant forces this match to be
// updated, so we never silently fall through to a default code.
fn exit_code_for(err: &WalletError) -> i32 {
    match err.category() {
        ErrorCategory::UserInput => 64,   // EX_USAGE — bad invocation
        ErrorCategory::Config => 78,      // EX_CONFIG — config error
        ErrorCategory::Rpc => 69,         // EX_UNAVAILABLE — service unavailable
        ErrorCategory::Crypto => 77,      // EX_NOPERM — permission/credentials
        ErrorCategory::Storage => 74,     // EX_IOERR — i/o error
        ErrorCategory::Sync => 75,        // EX_TEMPFAIL — transient failure
        ErrorCategory::Transaction => 65, // EX_DATAERR — bad data
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let result = match args.command {
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
            is_send_all,
            from_addresses,
            use_existing_change_address,
            max_fee_rate,
            exact_fee_rate: fee_rate,
            max_fee,
            password,
            show_transactions,
            payload,
        } => {
            commands::send(
                &daemon_address,
                &to_address,
                send_amount.as_deref(),
                is_send_all,
                from_addresses,
                use_existing_change_address,
                max_fee_rate,
                fee_rate,
                max_fee,
                password,
                show_transactions,
                payload.as_deref(),
            )
            .await
        }

        Commands::CreateUnsignedTransaction {
            daemon_address,
            to_address,
            send_amount,
            is_send_all,
            from_addresses,
            use_existing_change_address,
            max_fee_rate,
            exact_fee_rate,
            max_fee,
            payload,
        } => {
            commands::create_unsigned_transaction(
                &daemon_address,
                &to_address,
                send_amount.as_deref(),
                is_send_all,
                from_addresses,
                use_existing_change_address,
                max_fee_rate,
                exact_fee_rate,
                max_fee,
                payload.as_deref(),
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

        Commands::AddressBalances { daemon_address } => {
            commands::address_balances(&daemon_address).await
        }
    };

    if let Err(e) = result {
        eprintln!(
            "Error [{}/{}] at {}: {}",
            e.category(),
            e.kind_name(),
            e.location(),
            e.user_message()
        );
        process::exit(exit_code_for(&e));
    }
}
