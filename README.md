# Chia Seeder Light

Super light chia peer crawler and DNS seeder written in Rust based on [chia-wallet-sdk](https://github.com/xch-dev/chia-wallet-sdk). Doesn't hog CPU or memory like the reference seeder in [chia-blockchain](https://github.com/Chia-Network/chia-blockchain).

## Installation

Download the binary from [releases](https://github.com/dexie-space/chia-seeder-light/releases) or install via cargo:

```
cargo install chia-seeder-light
```

```bash
Usage: chia-seeder-light [OPTIONS] --domain <domain>

Options:
  -n, --network-id <network id>   Set network id [default: mainnet]
  -l, --listen-address <ip:port>  Set listen address [default: [::]:53]
  -d, --domain <domain>           Set seeder domain (eg. seeder.dexie.space.), Important: must end with a dot
  -e, --entry-node <ip:port>      Set initial entry node, will not use DNS to find peers (eg. 203.0.113.23:8444)
  -h, --help                      Print help
  -V, --version                   Print version
```

Note: To operate on port 53, chia-seeder-light must run as root. Alternatively configure firewall rules to forward traffic to the appropriate port.

## How it works

This seeder takes a graceful approach to crawling the network. On startup, it either uses a specified entry node or discovers initial peers through other DNS seeders. It then connects to each discovered peer using the chia protocol to verify they're reachable and requests their peer list.

Discovered peers and their status (reachable/unreachable) are tracked in a SQLite database and periodically rechecked.

Take a look at the [config.rs](src/config.rs) to fine tune the behavior according to your needs.
