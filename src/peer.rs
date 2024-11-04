use crate::config::*;
use crate::dns::*;

use chia_wallet_sdk::{connect_peer, Connector};
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::{
    collections::{HashSet, VecDeque},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::time::timeout;
use tracing::info;

pub fn start_peer_crawler(
    initial_peers: Vec<SocketAddr>,
    tls: Connector,
    authority: Arc<RandomizedAuthority>,
    network_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let tls = Arc::new(tls);
        let mut seen_peers = HashSet::new();
        let mut peers_to_process = VecDeque::new();
        let mut active_tasks = FuturesUnordered::new();

        for peer in initial_peers {
            if seen_peers.insert(peer) {
                peers_to_process.push_back(peer);
            }
        }

        loop {
            while active_tasks.len() < MAX_CONCURRENT_TASKS {
                if let Some(peer) = peers_to_process.pop_front() {
                    let tls = Arc::clone(&tls);
                    let authority = Arc::clone(&authority);
                    active_tasks.push(process_peer(peer, tls, authority, network_id.clone()));
                } else {
                    break;
                }
            }

            if active_tasks.is_empty() && peers_to_process.is_empty() {
                break;
            }

            if let Some(new_peers) = active_tasks.next().await {
                for peer in new_peers {
                    if seen_peers.insert(peer) {
                        peers_to_process.push_back(peer);
                    }
                }
            }
        }
    })
}

async fn process_peer(
    peer_addr: SocketAddr,
    tls: Arc<Connector>,
    authority: Arc<RandomizedAuthority>,
    network_id: String,
) -> Vec<SocketAddr> {
    let mut new_peers = Vec::new();

    match timeout(
        Duration::from_secs(PEER_TIMEOUT),
        connect_peer(network_id, (*tls).clone(), peer_addr),
    )
    .await
    {
        Ok(Ok((peer, ws_stream))) => {
            // Get more peers and collect them, only consider a peer up if we can get more peers from it
            if let Ok(response) = peer.request_peers().await {
                println!("Adding reachable peer: {:?}", peer_addr);

                // Add to DNS
                authority.add_peer(peer.socket_addr()).await;

                for peer_info in response.peer_list {
                    if let Ok(ip) = peer_info.host.parse() {
                        let addr = SocketAddr::new(ip, peer_info.port);
                        new_peers.push(addr);
                    }
                }
            }

            // Clean up
            drop(ws_stream);
            drop(peer);
        }
        Ok(Err(e)) => {
            info!("Failed to connect to peer {}: {}", peer_addr, e);
        }
        Err(_) => {
            info!("Timeout connecting to peer {}", peer_addr);
        }
    }

    new_peers
}

pub async fn start_peer_rechecker(
    tls: Connector,
    authority: Arc<RandomizedAuthority>,
    network_id: String,
) -> anyhow::Result<()> {
    let tls = Arc::new(tls);

    loop {
        tokio::time::sleep(RECHECK_INTERVAL).await;

        let peers = authority.get_peers().await;

        println!(
            "Starting periodic peer recheck, checking {} reachable peers",
            peers.len()
        );

        let mut tasks = FuturesUnordered::new();
        let mut remaining_peers = VecDeque::from_iter(peers);

        while !remaining_peers.is_empty() || !tasks.is_empty() {
            while tasks.len() < MAX_CONCURRENT_TASKS && !remaining_peers.is_empty() {
                if let Some(addr) = remaining_peers.pop_front() {
                    let tls = Arc::clone(&tls);
                    let authority = Arc::clone(&authority);
                    let network_id = network_id.clone();

                    tasks.push(async move {
                        let result = timeout(
                            Duration::from_secs(PEER_TIMEOUT),
                            connect_peer(network_id, (*tls).clone(), addr),
                        )
                        .await;

                        if !matches!(result, Ok(Ok((_, _)))) {
                            println!("Removing unreachable peer: {:?}", addr);
                            authority.remove_peer(addr).await;
                        }
                    });
                }
            }

            // Wait for at least one task to complete
            if !tasks.is_empty() {
                tasks.next().await;
            }
        }
    }
}
