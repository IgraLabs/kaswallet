# kaswallet-batch-send

Standalone CLI that sends KAS to a list of recipients directly through a
kaspad node — as few transactions as the recipients fit into, all in **one
connected session**. No running `kaswallet-daemon` is required — the wallet
keys file (created by `kaswallet-create`) is passed directly, everything is
signed locally, and private keys never leave the machine.

```bash
kaswallet-batch-send --testnet \
  -o 'kaspatest:qq...:1.5' \
  -o 'kaspatest:qz...:2' \
  [-k ~/.kaswallet/testnet-10/keys.json] \
  [-p <password>] \
  [--dry-run]
```

## What it does

1. Offline validation first (fail fast): parses every `--output`, loads the
   keys file, verifies the password — before any network I/O.
2. Connects to kaspad (`--server`, default: localhost with the network's
   default gRPC port) and sanity-checks it: right network, `--utxoindex`
   enabled, synced.
3. One-shot wallet sync: scans the wallet's addresses and fetches spendable
   UTXOs (including mempool-spent exclusion, immature-coinbase filtering).
4. Packs recipients into transactions — as many outputs per transaction as
   the KIP-9 storage-mass / standardness limits allow (`--max-outputs-per-tx`
   caps it) — using the same fee/mass accounting the daemon uses. Small
   amounts force one recipient per transaction; large amounts fit many.
5. Signs and submits each transaction, then chains the next one onto its
   change **without reconnecting or re-syncing** (the change is registered in
   the local UTXO view; kaspad accepts the child spending the mempool parent).
   Prints a `paid` line per recipient. This is what makes a 20-recipient
   fleet top-up take one connect + one sync instead of 20.

Because it all happens in one session, a large fleet payout is ~10-20× faster
than looping a fresh process per recipient.

## Checking a wallet (no daemon, no password)

```bash
kaswallet-batch-send --testnet --show-address
# kaspatest:qq...            <- derived offline; works with no node at all

kaswallet-batch-send --testnet --show-balance
# spendable_kas: 12.50000000  <- requires a reachable node
# total_kas: 12.50000000
```

`--show-address` prints the wallet's receive address (external keychain,
index 0 — always inside the wallet's scan window), derived from the keys
file's public key. `--show-balance` syncs the wallet view against the node
and prints spendable vs total. Both flags work without the password and can
be combined.

## Flags

| Flag | Meaning |
|---|---|
| `--testnet` / `--devnet` / `--simnet` | network selection (`--testnet-suffix`, default 10) |
| `-k, --keys <path>` | keys file (default `~/.kaswallet/<network>/keys.json`) |
| `-s, --server <url>` | kaspad gRPC endpoint, e.g. `grpc://host:16110` |
| `-o, --output <address>:<amount-KAS>` | recipient; repeatable; amount split on the LAST `:` |
| `-F, --outputs-file <path>` | JSON file with recipients; mutually exclusive with `-o` |
| `-p, --password <pw>` | wallet password; falls back to the `KASWALLET_PASSWORD` env var, then an interactive prompt |
| `--fee-rate-max` / `--fee-rate-exact` / `--fee-max` | fee control, mutually exclusive (default: node priority feerate, fee capped at 1 KAS) |
| `-u, --use-existing-change-address` | reuse internal index 0 instead of allocating a new change address |
| `--max-outputs-per-tx <N>` | cap recipients packed per transaction (default 100); the tool auto-shrinks below it as the mass limit requires and chains the remainder |
| `--dry-run` | full plan (connect, sync, select, estimate) — chains locally, nothing signed or submitted |
| `--show-address` / `--show-balance` | wallet checks, see above |
| `--allow-unsynced-node` | proceed when the node reports it is not synced (isolated simnet nodes) |
| `-v, --verbose` | debug-level logs (`RUST_LOG` also honored) |
| `-q, --quiet` | warnings/errors only on stderr; the stdout summary is unaffected — for scripted loops |

## Outputs file

`--outputs-file payouts.json` reads the recipients from a JSON **map** of
`"address": "amount-KAS"` entries:

```json
{
  "kaspatest:qq...": "1.5",
  "kaspatest:qz...": "2"
}
```

Amounts must be JSON **strings** — JSON numbers are floats and cannot
represent all 8-decimal KAS values exactly. Entries are processed in
address-sorted order. A map cannot express duplicate recipient addresses;
use repeated `--output` pairs when you need two outputs to the same address.

## Output contract

- **stderr** — the stage-by-stage narrative: node/server info, the wallet's
  receive address, per-address spendable balances, an explicit
  requested-vs-spendable balance check, the sending (source) addresses with
  per-address contributions, per-recipient output lines (change marked),
  fee and compute/transient/storage masses, and the submitted tx id.
- **stdout** — machine-parseable, one `paid` line per recipient plus a
  summary (a caller can record exactly who was paid and resume the rest):

  ```
  paid kaspa:qq...aaa 0.20000000 <tx_id>
  paid kaspa:qz...bbb 0.20000000 <tx_id>
  ...
  summary: 20/20 recipients paid, 20 transaction(s), total_fee_kas 0.04116000
  ```

  Under `--dry-run` each `paid` line is tagged `(dry-run)` and carries the
  locally-computed tx id. On a mid-chain failure the already-`paid` lines are
  still printed, then `unpaid <address> <amount-KAS>` for the remainder, and
  the exit code is non-zero — so a rerun (or the fleet script) can finish only
  the unpaid recipients.

- **exit codes** (sysexits-style, shared with `kaswallet-cli`): 0 all paid,
  64 bad input, 65 transaction error (e.g. insufficient funds, mass exceeded),
  69 node/RPC unavailable, 74 I/O, 75 transient sync failure, 77 wrong
  password/credentials, 78 config. Actionable hints accompany
  `InsufficientFunds`, `MassExceeded`, and `FeeTooLow`.

## Non-interactive use

For scripts and cron, provide the password without a prompt via the
`KASWALLET_PASSWORD` environment variable (an explicit `-p` still wins):

```bash
export KASWALLET_PASSWORD='...'          # e.g. sourced from a chmod-600 .env file
kaswallet-batch-send --testnet -F payouts.json
```

The environment variable is preferable to `-p` on shared machines — command
lines are visible in `ps`, environment variables of your own processes are
not (to other users).

## Limits and operational notes

- Recipients are packed into as few transactions as the mass limit allows and
  the rest are chained in the same session. Only a **single** recipient whose
  output can't fit any transaction (true dust — KIP-9 storage mass grows as
  ~10^12 / amount-in-sompi per output) fails, with a typed `MassExceeded`;
  send a larger per-output amount or consolidate UTXOs first.
- The tool writes `keys.json` (last-used address indexes), like every other
  kaswallet binary. Do not run it against a keys file a live daemon is
  actively using.
- Native subnetwork only; fully-signable wallets only (multisig cosigning
  is rejected with `NotFullySigned` before anything is submitted).
- Mainnet is disabled repo-wide until production readiness; the hidden
  pre-launch escape hatch mirrors the other binaries.

## Build and test

```bash
cargo build -p kaswallet-batch-send                  # binary at target/debug/kaswallet-batch-send
cargo test -p kaswallet-batch-send                   # unit tests (args + output parsing)
cargo test -p kaswallet-test-integration \
  --features integration-tests batch_send            # end-to-end against a spawned simnet kaspad
```

`./install.sh` at the repo root installs it to `~/.cargo/bin` along with the
other kaswallet binaries.
