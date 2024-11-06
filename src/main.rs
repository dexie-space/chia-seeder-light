use chia_wallet_sdk::{create_rustls_connector, load_ssl_cert, Network};
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};
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
        value_name = "ip:port",
        help = "Set listen address",
        default_value = "[::]:53"
    )]
    listen_address: SocketAddr,

    #[clap(
        long,
        short,
        value_name = "domain",
        help = "Set seeder domain (eg. seeder.dexie.space.), Important: must end with a dot"
    )]
    domain: String,

    #[clap(
        long,
        short,
        value_name = "ip:port",
        help = "Set initial entry node, will not use DNS to find peers (eg. 203.0.113.23:8444)"
    )]
    entry_node: Option<SocketAddr>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info")
                    .add_directive("trust_dns_server=warn".parse().unwrap())
            }),
        )
        .init();

    let opt = Opt::parse();
    let tls = create_rustls_connector(&load_ssl_cert("wallet.crt", "wallet.key")?)?;

    let network = if opt.network_id == "mainnet" {
        Network::default_mainnet()
    } else if opt.network_id == "testnet11" {
        Network::default_testnet11()
    } else {
        error!("Error: Unknown network id: {}", opt.network_id);
        std::process::exit(1);
    };

    // DNS zone setup
    let zone_name = Name::parse(&opt.domain, None)?;
    let authority = Arc::new(PeerDiscoveryAuthority::new(zone_name.clone()));

    let mut catalog = Catalog::new();
    catalog.upsert(zone_name.clone().into(), Box::new(authority.clone()));

    // use entry node
    let peers = if let Some(entry_node) = opt.entry_node {
        info!("Using entry node: {}", entry_node);
        vec![entry_node]
    } else {
        info!("Looking up initial peers...");
        let peers = Network::lookup_all(&network, Duration::from_secs(10), 10).await;
        info!("Found {} initial peers", peers.len());
        peers
    };

    let crawler_handle = start_peer_crawler(
        peers,
        tls.clone(),
        authority.clone(),
        opt.network_id.clone(),
    );
    let rechecker_handle = start_peer_rechecker(tls, authority.clone(), opt.network_id);
    let server_handle = start_dns_server(catalog, opt.listen_address).await?;

    tokio::select! {
        Err(e) = server_handle => error!("DNS server failed: {e}"),
        Err(e) = crawler_handle => error!("Crawler failed: {e}"),
        Err(e) = rechecker_handle => error!("Rechecker failed: {e}"),
    };

    Ok(())
}
