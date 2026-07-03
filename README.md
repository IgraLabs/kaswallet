# kaswallet

A gRPC-based Kaspa wallet daemon and cli client.

## Warning

This is pre-alpha software, not yet intended for use with real money.  
Code is provided for information and review purposes only.

Usage in mainnet is disabled until this software is production ready.

## Installation

Rust is required.

```bash
./install.sh
```

Will install all the kaswallet binaries to your `~/.cargo/bin` folder.  
Make sure it's in your PATH to make them accessible from anywhere.

## Usage

### Setup

To set up a wallet on your computer do one of the following:

#### New Wallet

```bash
kaswallet-create \\
  [--testnet/--devnet/--simnet]  # or omit for mainnet - disabled until production ready
```

This will create a new wallet keys file at `~/.kaswallet/[mainnet/testnet-10/devnet/simnet]/keys.json`.  
Use `--keys [path_to_keys_file]` to specify a custom location.

You will be asked for a password (leave blank for no password), and then your mnemonic will be printed.  
Write down this mnemonic and store it in a safe place.

#### Import Mnemonic

```bash
kaswallet-create \\
  [--testnet/--devnet/--simnet] \\
  --import
```

This will let you input your mnemonic rather than generate a new wallet.

-----
See `kaswallet-create --help` for further options.

## Start Daemon

Once `kaswallet-create` has run and a keys file was generated, you can start `kaswallet-daemon`:

```bash
kaswallet-daemon \\
  [--testnet/--devnet/--simnet] \\
  [--server='grpc://<ip>:<port>'] # Kaspad GRPC endpoint. Optional. Defaults to localhost with the default port for given network
  [--listen='<ip>:<port>']        # Interface and port to listen on. Optional. Defaults to 127.0.0.1:8082.
```

Keep this process running for as long as you want wallet services available.

See `kaswallet-daemon --help` for further options.

## Cli client

```bash
kaswallet-cli [command] [arguments]
```

Available commands are:

```
  balance                      Shows the balance of the wallet
  show-addresses               Shows all generated public addresses of the current wallet
  new-address                  Generates a new public address of the current wallet
  get-utxos                    Get UTXOs for the wallet
  send                         Sends a Kaspa transaction to a public address
  create-unsigned-transaction  Create an unsigned Kaspa transaction
  sign                         Sign the given unsigned transaction(s)
  broadcast                    Broadcast the given signed transaction(s)
  get-daemon-version           Get the wallet daemon version
  address-balances             Show balance per address with UTXO details as JSON
  help                         Print this message or the help of the given subcommand(s)
```

See `kaswallet-cli [command] --help` for available arguments for each command.

## Batch send (standalone, no daemon)

`kaswallet-batch-send` sends KAS to a list of addresses in ONE multi-output
transaction, talking directly to a kaspad node — no running `kaswallet-daemon`
required. Keys are passed directly (the `keys.json` created by
`kaswallet-create`).

```bash
kaswallet-batch-send \
  [--testnet/--devnet/--simnet] \
  [--keys <path_to_keys_file>] \          # defaults to ~/.kaswallet/<network>/keys.json
  [--server 'grpc://<ip>:<port>'] \       # kaspad gRPC endpoint (default: localhost)
  --output <address>:<amount-KAS> \       # repeatable, e.g. -o 'kaspatest:qq...:1.5'
  [--output <address>:<amount-KAS>] ... \ # or -F payouts.json ({"address": "amount-KAS", ...})
  [-p <password>] \                       # prompted when omitted
  [--dry-run]                             # full plan + logs, nothing submitted
```

The stage-by-stage narrative (node info, the wallet's receive address,
spendable balance per address, an explicit balance check, the sending
addresses, per-recipient outputs, change, fee and masses, tx id) goes to
stderr; stdout carries only a short parseable summary. Exit codes follow the
same sysexits-style mapping as `kaswallet-cli`. Fee control matches
`kaswallet-cli send` (`--fee-rate-max`, `--fee-rate-exact`, `--fee-max`;
default: node priority feerate, capped at 1 KAS).

To check a wallet without the daemon (and without a password):

```bash
kaswallet-batch-send --testnet --show-address
# kaspatest:qq...                      <- offline, works with no node at all

kaswallet-batch-send --testnet --show-balance
# spendable_kas: 12.50000000           <- needs a reachable node (--server)
# total_kas: 12.50000000
```

The flags combine (`--show-address --show-balance` prints all three lines).

Notes:

- One transaction per run. A batch that exceeds the standard mass limits fails
  with a clear `MassExceeded` error — fewer recipients per run, larger
  per-output amounts (KIP-9 storage mass grows as ~10^12 / amount-in-sompi per
  output), or consolidate UTXOs first.
- The tool writes `keys.json` (last-used address indexes), like every other
  kaswallet binary. Do not run it against a keys file a live daemon is
  actively using.
- Native subnetwork and fully-signable (non-cosigned) wallets only.
- `--allow-unsynced-node` skips the node-synced check (isolated simnet nodes).

See `kaswallet-batch-send --help` for all options.
