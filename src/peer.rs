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
use tracing::{debug, info};

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
                    let network_id = network_id.clone();
                    active_tasks.push(async move {
                        match handle_peer_connection(peer, tls, authority, network_id).await {
                            Ok(new_peers) => new_peers,
                            Err(_) => Vec::new(),
                        }
                    });
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

async fn handle_peer_connection(
    addr: SocketAddr,
    tls: Arc<Connector>,
    authority: Arc<RandomizedAuthority>,
    network_id: String,
) -> Result<Vec<SocketAddr>, ()> {
    if authority.is_blocked(&addr) {
        debug!("Skipping blocked peer: {:?}", addr);
        return Err(());
    }

    let result = timeout(Duration::from_secs(PEER_TIMEOUT), async {
        let (peer, ws_stream) = connect_peer(network_id, (*tls).clone(), addr).await?;
        let response = peer.request_peers().await?;

        authority.add_peer(peer.socket_addr()).await;

        let mut new_peers = Vec::new();
        for peer_info in response.peer_list {
            if let Ok(ip) = peer_info.host.parse() {
                if addr.port() == peer_info.port {
                    new_peers.push(SocketAddr::new(ip, peer_info.port));
                }
            }
        }

        drop(ws_stream);
        drop(peer);

        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(new_peers)
    })
    .await;

    match result {
        Ok(Ok(new_peers)) => Ok(new_peers),
        Ok(Err(e)) => {
            debug!("Error for peer {}: {:?}", addr, e);
            authority.block_peer(addr, Duration::from_secs(PEER_BLOCKLIST_TTL));
            Err(())
        }
        Err(_) => {
            debug!("Timeout for peer {}", addr);
            authority.block_peer(addr, Duration::from_secs(PEER_BLOCKLIST_TTL));
            Err(())
        }
    }
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

        info!(
            "Starting periodic peer recheck, checking {} reachable peers",
            peers.len()
        );

        authority.cleanup_blocklist();

        let mut tasks = FuturesUnordered::new();
        let mut remaining_peers = VecDeque::from_iter(peers);

        while !remaining_peers.is_empty() || !tasks.is_empty() {
            while tasks.len() < MAX_CONCURRENT_TASKS && !remaining_peers.is_empty() {
                if let Some(addr) = remaining_peers.pop_front() {
                    let tls = Arc::clone(&tls);
                    let authority = Arc::clone(&authority);
                    let network_id = network_id.clone();

                    tasks.push(async move {
                        if handle_peer_connection(addr, tls, authority.clone(), network_id)
                            .await
                            .is_err()
                        {
                            authority.remove_peer(addr).await;
                        }
                    });
                }
            }

            if !tasks.is_empty() {
                tasks.next().await;
            }
        }

        info!("Periodic peer recheck finished");
    }
}
