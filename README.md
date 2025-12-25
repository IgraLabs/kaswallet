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
  help                         Print this message or the help of the given subcommand(s)
```

See `kaswallet-cli [command] --help` for available arguments for each command.
