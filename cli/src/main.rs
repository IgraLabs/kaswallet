use args::{Args, Commands};
use clap::Parser;
use std::process;

mod args;
mod commands;
mod utils;

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
            send_all,
            from_addresses,
            use_existing_change_address,
            max_fee_rate,
            fee_rate,
            max_fee,
            password,
            show_serialized,
            payload,
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
                payload.as_deref(),
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
            payload,
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
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
