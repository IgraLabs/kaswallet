use crate::utils::{format_kas, kas_to_sompi};
use common::model::WalletSignableTransaction;
use kaswallet_client::client::KaswalletClient;
use proto::kaswallet_proto::{fee_policy, FeePolicy};
use std::fs;
use std::io::{self, Write};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn connect(daemon_address: &str) -> Result<KaswalletClient> {
    KaswalletClient::connect(daemon_address.to_string())
        .await
        .map_err(|e| format!("Failed to connect to daemon at {}: {}", daemon_address, e).into())
}

/// Get and display the wallet balance
pub async fn balance(daemon_address: &str, verbose: bool) -> Result<()> {
    let mut client = connect(daemon_address).await?;

    let balance_info = client.get_balance(verbose).await?;

    let pending_suffix = if balance_info.pending > 0 && !verbose {
        " (pending)"
    } else {
        ""
    };

    if verbose {
        println!(
            "Address                                                                       Available             Pending"
        );
        println!(
            "-----------------------------------------------------------------------------------------------------------"
        );
        for addr_balance in &balance_info.address_balances {
            println!(
                "{} {} {}",
                addr_balance.address,
                format_kas(addr_balance.available),
                format_kas(addr_balance.pending)
            );
        }
        println!(
            "-----------------------------------------------------------------------------------------------------------"
        );
        print!("                                                 ");
    }

    println!(
        "Total balance, KAS {} {}{}",
        format_kas(balance_info.available),
        format_kas(balance_info.pending),
        pending_suffix
    );

    Ok(())
}

/// Show all generated addresses
pub async fn show_addresses(daemon_address: &str) -> Result<()> {
    let mut client = connect(daemon_address).await?;

    let addresses = client.get_addresses().await?;

    println!("Addresses ({}):", addresses.len());
    for address in &addresses {
        println!("{}", address);
    }

    println!();
    println!(
        "Note: the above are only addresses that were manually created by the 'new-address' command. \
         If you want to see a list of all addresses, including change addresses, \
         that have a positive balance, use the command 'balance -v'"
    );

    Ok(())
}

/// Generate a new address
pub async fn new_address(daemon_address: &str) -> Result<()> {
    let mut client = connect(daemon_address).await?;

    let address = client.new_address().await?;

    println!("New address: {}", address);

    Ok(())
}

/// Get the daemon version
pub async fn get_daemon_version(daemon_address: &str) -> Result<()> {
    let mut client = connect(daemon_address).await?;

    let version = client.get_version().await?;

    println!("Daemon version: {}", version);

    Ok(())
}

/// Get UTXOs for the wallet
pub async fn get_utxos(
    daemon_address: &str,
    addresses: Vec<String>,
    include_pending: bool,
    include_dust: bool,
) -> Result<()> {
    let mut client = connect(daemon_address).await?;

    let address_utxos = client
        .get_utxos(addresses, include_pending, include_dust)
        .await?;

    for addr_utxos in &address_utxos {
        println!("Address: {}", addr_utxos.address);
        println!("  UTXOs ({}):", addr_utxos.utxos.len());

        for utxo in &addr_utxos.utxos {
            let flags = [
                if utxo.is_coinbase {
                    Some("coinbase")
                } else {
                    None
                },
                if utxo.is_pending {
                    Some("pending")
                } else {
                    None
                },
                if utxo.is_dust { Some("dust") } else { None },
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");

            let flags_str = if flags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", flags)
            };

            println!(
                "    {}:{} - {} KAS{}",
                utxo.outpoint.transaction_id,
                utxo.outpoint.index,
                format_kas(utxo.amount).trim(),
                flags_str
            );
        }
        println!();
    }

    Ok(())
}

fn build_fee_policy(
    max_fee_rate: Option<f64>,
    fee_rate: Option<f64>,
    max_fee: Option<u64>,
) -> Option<FeePolicy> {
    if let Some(rate) = fee_rate {
        Some(FeePolicy {
            fee_policy: Some(fee_policy::FeePolicy::ExactFeeRate(rate)),
        })
    } else if let Some(rate) = max_fee_rate {
        Some(FeePolicy {
            fee_policy: Some(fee_policy::FeePolicy::MaxFeeRate(rate)),
        })
    } else if let Some(fee) = max_fee {
        Some(FeePolicy {
            fee_policy: Some(fee_policy::FeePolicy::MaxFee(fee)),
        })
    } else {
        None
    }
}

fn get_password(prompt: &str, password: Option<String>) -> Result<String> {
    if let Some(p) = password {
        Ok(p)
    } else {
        print!("{}", prompt);
        io::stdout().flush()?;
        rpassword::read_password().map_err(|e| e.into())
    }
}

/// Send funds to an address
#[allow(clippy::too_many_arguments)]
pub async fn send(
    daemon_address: &str,
    to_address: &str,
    send_amount: Option<&str>,
    send_all: bool,
    from_addresses: Vec<String>,
    use_existing_change_address: bool,
    max_fee_rate: Option<f64>,
    fee_rate: Option<f64>,
    max_fee: Option<u64>,
    password: Option<String>,
    show_serialized: bool,
) -> Result<()> {
    // Validate that either send_amount or send_all is specified
    if send_amount.is_none() && !send_all {
        return Err("Exactly one of '--send-amount' or '--send-all' must be specified".into());
    }

    let mut client = connect(daemon_address).await?;

    let amount_sompi = if let Some(amount_str) = send_amount {
        kas_to_sompi(amount_str)?
    } else {
        0
    };

    let fee_policy = build_fee_policy(max_fee_rate, fee_rate, max_fee);

    let password = get_password("Password: ", password)?;

    let result = client
        .send(
            to_address.to_string(),
            amount_sompi,
            send_all,
            Vec::new(), // payload
            from_addresses,
            Vec::new(), // utxos
            use_existing_change_address,
            fee_policy,
            password,
        )
        .await?;

    println!(
        "Broadcasted {} transaction(s)",
        result.transaction_ids.len()
    );
    println!("Transaction ID(s):");
    for tx_id in &result.transaction_ids {
        println!("  {}", tx_id);
    }

    if show_serialized {
        println!();
        println!("Serialized Transaction(s):");
        for tx in &result.signed_transactions {
            let serialized = serialize_transaction(tx);
            println!("  {}", serialized);
            println!();
        }
    }

    Ok(())
}

/// Create unsigned transactions
#[allow(clippy::too_many_arguments)]
pub async fn create_unsigned_transaction(
    daemon_address: &str,
    to_address: &str,
    send_amount: Option<&str>,
    send_all: bool,
    from_addresses: Vec<String>,
    use_existing_change_address: bool,
    max_fee_rate: Option<f64>,
    fee_rate: Option<f64>,
    max_fee: Option<u64>,
) -> Result<()> {
    // Validate that either send_amount or send_all is specified
    if send_amount.is_none() && !send_all {
        return Err("Exactly one of '--send-amount' or '--send-all' must be specified".into());
    }

    let mut client = connect(daemon_address).await?;

    let amount_sompi = if let Some(amount_str) = send_amount {
        kas_to_sompi(amount_str)?
    } else {
        0
    };

    let fee_policy = build_fee_policy(max_fee_rate, fee_rate, max_fee);

    let unsigned_transactions = client
        .create_unsigned_transactions(
            to_address.to_string(),
            amount_sompi,
            send_all,
            Vec::new(), // payload
            from_addresses,
            Vec::new(), // utxos
            use_existing_change_address,
            fee_policy,
        )
        .await?;

    println!(
        "Created {} unsigned transaction(s)",
        unsigned_transactions.len()
    );
    println!("Unsigned Transaction(s) (hex encoded):");
    for tx in &unsigned_transactions {
        let serialized = serialize_transaction(tx);
        println!("{}", serialized);
        println!();
    }

    Ok(())
}

/// Sign unsigned transactions
pub async fn sign(
    daemon_address: &str,
    transaction: Option<String>,
    transaction_file: Option<String>,
    password: Option<String>,
) -> Result<()> {
    let transactions_hex = get_transactions_hex(transaction, transaction_file)?;
    let unsigned_transactions = parse_transactions_hex(&transactions_hex)?;

    let mut client = connect(daemon_address).await?;

    let password = get_password("Password: ", password)?;

    let signed_transactions = client.sign(unsigned_transactions, password).await?;

    println!("Signed {} transaction(s)", signed_transactions.len());
    println!("Signed Transaction(s) (hex encoded):");
    for tx in &signed_transactions {
        let serialized = serialize_transaction(tx);
        println!("{}", serialized);
        println!();
    }

    Ok(())
}

/// Broadcast signed transactions
pub async fn broadcast(
    daemon_address: &str,
    transaction: Option<String>,
    transaction_file: Option<String>,
) -> Result<()> {
    let transactions_hex = get_transactions_hex(transaction, transaction_file)?;
    let transactions = parse_transactions_hex(&transactions_hex)?;

    let mut client = connect(daemon_address).await?;

    let tx_ids = client.broadcast(transactions).await?;

    println!("Broadcasted {} transaction(s)", tx_ids.len());
    println!("Transaction ID(s):");
    for tx_id in &tx_ids {
        println!("  {}", tx_id);
    }

    Ok(())
}

fn get_transactions_hex(
    transaction: Option<String>,
    transaction_file: Option<String>,
) -> Result<String> {
    if let Some(tx) = transaction {
        Ok(tx)
    } else if let Some(file_path) = transaction_file {
        fs::read_to_string(&file_path)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("Failed to read transaction file '{}': {}", file_path, e).into())
    } else {
        Err("Either --transaction or --transaction-file must be specified".into())
    }
}

fn parse_transactions_hex(hex_str: &str) -> Result<Vec<WalletSignableTransaction>> {
    // Each transaction is on a separate line
    let mut transactions = Vec::new();

    for line in hex_str.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let bytes =
            hex::decode(line).map_err(|e| format!("Invalid hex in transaction: {}", e))?;

        let tx: WalletSignableTransaction = borsh::from_slice(&bytes)
            .map_err(|e| format!("Failed to deserialize transaction: {}", e))?;

        transactions.push(tx);
    }

    if transactions.is_empty() {
        return Err("No transactions found".into());
    }

    Ok(transactions)
}

fn serialize_transaction(tx: &WalletSignableTransaction) -> String {
    let bytes = borsh::to_vec(tx).expect("Failed to serialize transaction");
    hex::encode(bytes)
}
