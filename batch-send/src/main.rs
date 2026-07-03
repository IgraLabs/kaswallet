use batch_send::args::Args;
use clap::Parser;
use common::amount::kas_display;
use common::errors::{TransactionError, WalletError};
use std::process;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Logs (the stage-by-stage narrative) go to stderr; stdout carries only
    // the final machine-parseable summary — the script contract.
    //
    // At the default level the daemon crate's transaction-generator and
    // sync-manager internals are demoted to warn: the runner's own narrative
    // already reports the wallet state and the built transaction, and the
    // generator logs its fee-estimation MOCK builds at info, which reads as
    // a confusing duplicate "built unsigned tx" line. RUST_LOG overrides.
    let default_filter = if args.verbose {
        "debug".to_string()
    } else {
        let level = if args.quiet { "warn" } else { "info" };
        format!(
            "{level},kaswallet_daemon::transaction_generator=warn,kaswallet_daemon::sync_manager=warn"
        )
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&default_filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    if args.show_address || args.show_balance {
        // The address is offline-derivable — printed first so it is
        // available even when the node (needed for the balance) is down.
        if args.show_address {
            match batch_send::show_receive_address(&args).await {
                Ok(address) => println!("{address}"),
                Err(e) => exit_with_error(e),
            }
        }
        if args.show_balance {
            match batch_send::wallet_balance(&args).await {
                Ok(balance) => {
                    println!("spendable_kas: {}", kas_display(balance.spendable_sompi));
                    println!("total_kas: {}", kas_display(balance.total_sompi));
                }
                Err(e) => exit_with_error(e),
            }
        }
        return;
    }

    match batch_send::run(args).await {
        Ok(summary) => {
            // Machine-readable per-recipient result on stdout: `paid` lines
            // let a caller (e.g. the fleet script) record exactly who was
            // paid and resume the rest on a later run.
            let dry_tag = if summary.dry_run { " (dry-run)" } else { "" };
            for recipient in &summary.paid {
                println!(
                    "paid {} {} {}{dry_tag}",
                    recipient.address,
                    kas_display(recipient.amount_sompi),
                    recipient.tx_id,
                );
            }
            for (address, amount_sompi) in &summary.unpaid {
                println!("unpaid {} {}", address, kas_display(*amount_sompi));
            }
            println!(
                "summary: {}/{} recipients paid, {} transaction(s), total_fee_kas {}{}",
                summary.paid.len(),
                summary.recipients,
                summary.transaction_ids.len(),
                kas_display(summary.fee_sompi),
                if summary.dry_run { ", dry-run" } else { "" },
            );

            if let Some((reason, exit_code)) = &summary.failure {
                eprintln!("error: distribution stopped: {reason}");
                eprintln!(
                    "hint: {} recipient(s) were paid; rerun with the unpaid recipients to finish \
                     (the fleet script resumes automatically via its ledger).",
                    summary.paid.len()
                );
                process::exit(*exit_code);
            }
        }
        Err(e) => exit_with_error(e),
    }
}

fn exit_with_error(e: WalletError) -> ! {
    eprintln!(
        "Error [{}/{}] at {}: {}",
        e.category(),
        e.kind_name(),
        e.location(),
        e.user_message()
    );
    match &e {
        WalletError::Transaction(TransactionError::MassExceeded { mass, limit, .. }) => {
            eprintln!(
                "hint: the transaction exceeds the standard mass limit ({mass} > {limit} \
                         grams). Reduce the number of recipients per run, send larger per-output \
                         amounts (storage mass grows as ~10^12 / amount-in-sompi per output), or \
                         consolidate wallet UTXOs first. If this happens while sending nearly \
                         the whole balance, the tiny change output is the likely cause — adjust \
                         amounts so the change is larger (>= ~0.1 KAS) or matches exactly."
            );
        }
        WalletError::Transaction(TransactionError::InsufficientFunds {
            required_sompi,
            available_sompi,
            ..
        }) => {
            eprintln!(
                "hint: required {} KAS (incl. fee), spendable {} KAS — short by {} KAS.",
                kas_display(*required_sompi),
                kas_display(*available_sompi),
                kas_display(required_sompi.saturating_sub(*available_sompi)),
            );
        }
        WalletError::Transaction(TransactionError::FeeTooLow { .. }) => {
            eprintln!(
                "hint: the fee was capped below the node's relay floor (1 sompi/gram). \
                         If you passed --fee-max, raise it — a batch transaction needs roughly \
                         its mass in sompi as the minimum fee."
            );
        }
        _ => {}
    }
    process::exit(e.process_exit_code());
}
