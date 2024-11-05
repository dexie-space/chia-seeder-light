# Chia Seeder Light

Super light and barebones chia peer crawler and DNS seeder written in Rust based on [chia-wallet-sdk](https://github.com/xch-dev/chia-wallet-sdk). Doesn't hog CPU or memory like the reference seeder in [chia-blockchain](https://github.com/Chia-Network/chia-blockchain).

Work in progress, likely not production ready.

```bash
Usage: chia-seeder-light [OPTIONS] --domain <domain>

Options:
  -n, --network-id <network id>  Set network id [default: mainnet]
  -a, --address <ip>             Set listen address [default: 0.0.0.0]
  -p, --port <port>              Set listen port [default: 53]
  -d, --domain <domain>          Set seeder domain (eg. seeder.dexie.space.), Important: must end with a dot
  -e, --entry-node <ip:port>    Set initial entry node, will not use DNS to find peers (eg. 203.0.113.23:8444)
  -h, --help                     Print help
  -V, --version                  Print version
```

Note: To operate on port 53, chia-seeder-light must run as root. Alternatively, you can configure firewall rules to forward traffic to the appropriate port.
