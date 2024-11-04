use chia_wallet_sdk::{create_rustls_connector, load_ssl_cert, Network};
use clap::Parser;
use std::sync::Arc;
use std::time::Duration;
use trust_dns_server::authority::Catalog;
use trust_dns_server::proto::rr::Name;

use crate::dns::*;
use crate::peer::{start_peer_crawler, start_peer_rechecker};

mod config;
mod dns;
mod peer;

#[derive(Parser, Debug)]
#[clap(name = "chia-seeder-light", version = env!("CARGO_PKG_VERSION"))]
struct Opt {
    #[clap(
        long,
        short,
        value_name = "network id",
        help = "Set network id",
        default_value = "mainnet"
    )]
    network_id: String,

    #[clap(
        long,
        short,
        value_name = "ip",
        help = "Set listen address",
        default_value = "0.0.0.0"
    )]
    address: String,

    #[clap(
        long,
        short,
        value_name = "port",
        help = "Set listen port",
        default_value = "53"
    )]
    port: u16,

    #[clap(
        long,
        short,
        value_name = "domain",
        help = "Set seeder domain (eg. seeder.dexie.space.), Important: must end with a dot"
    )]
    domain: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();
    let tls = create_rustls_connector(&load_ssl_cert("wallet.crt", "wallet.key")?)?;

    let network = if opt.network_id == "mainnet" {
        Network::default_mainnet()
    } else if opt.network_id == "testnet11" {
        Network::default_testnet11()
    } else {
        eprintln!("Error: Unknown network id: {}", opt.network_id);
        std::process::exit(1);
    };

    // DNS zone setup
    let zone_name = Name::parse(&opt.domain, None)?;
    let authority = Arc::new(RandomizedAuthority::new(zone_name.clone()));

    let mut catalog = Catalog::new();
    catalog.upsert(zone_name.clone().into(), Box::new(authority.clone()));

    println!("Looking up initial peers...");
    let peers = Network::lookup_all(&network, Duration::from_secs(10), 10).await;
    println!("Found {} initial peers", peers.len());

    let crawler_handle = start_peer_crawler(
        peers,
        tls.clone(),
        authority.clone(),
        opt.network_id.clone(),
    );
    let rechecker_handle = start_peer_rechecker(tls, authority.clone(), opt.network_id);
    let server_handle = start_dns_server(catalog, opt.address, opt.port).await?;

    tokio::select! {
        Err(e) = server_handle => println!("DNS server failed: {e}"),
        Err(e) = crawler_handle => println!("Crawler failed: {e}"),
        Err(e) = rechecker_handle => println!("Rechecker failed: {e}"),
    };

    Ok(())
}
