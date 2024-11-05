use crate::config::*;
use crate::dns::*;

use chia_wallet_sdk::{connect_peer, Connector};
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::{
    collections::{HashSet, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::time::timeout;
use tracing::{debug, info};

struct BlockedPeer {
    addr: SocketAddr,
    expires_at: Instant,
}

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
        let blocked_peers = Arc::new(Mutex::new(Vec::<BlockedPeer>::new()));

        for peer in initial_peers {
            if seen_peers.insert(peer) {
                peers_to_process.push_back(peer);
            }
        }

        loop {
            // Clean expired entries
            {
                let mut blocklist = blocked_peers.lock().unwrap();
                blocklist.retain(|peer| peer.expires_at > Instant::now());
            }

            while active_tasks.len() < MAX_CONCURRENT_TASKS {
                if let Some(peer) = peers_to_process.pop_front() {
                    if blocked_peers
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|bp| bp.addr == peer)
                    {
                        debug!("Skipping blocked peer: {:?}", peer);
                        continue;
                    }

                    let tls = Arc::clone(&tls);
                    let authority = Arc::clone(&authority);
                    let blocklist = Arc::clone(&blocked_peers);
                    active_tasks.push(process_peer(
                        peer,
                        tls,
                        authority,
                        network_id.clone(),
                        blocklist,
                    ));
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
    let result = timeout(
        Duration::from_secs(PEER_TIMEOUT),
        connect_peer(network_id, (*tls).clone(), addr),
    )
    .await;

    match result {
        Ok(Ok((peer, ws_stream))) => {
            let mut new_peers = Vec::new();
            if let Ok(response) = peer.request_peers().await {
                authority.add_peer(peer.socket_addr()).await;

                for peer_info in response.peer_list {
                    if let Ok(ip) = peer_info.host.parse() {
                        // Only add peers that are on the same port as the peer we connect to
                        if addr.port() == peer_info.port {
                            new_peers.push(SocketAddr::new(ip, peer_info.port));
                        }
                    }
                }
            }
            drop(ws_stream);
            drop(peer);
            Ok(new_peers)
        }
        Ok(Err(e)) => {
            debug!("Connection error for peer {}: {:?}", addr, e);
            Err(())
        }
        Err(e) => {
            debug!("Timeout error for peer {}: {:?}", addr, e);
            Err(())
        }
    }
}

async fn process_peer(
    peer_addr: SocketAddr,
    tls: Arc<Connector>,
    authority: Arc<RandomizedAuthority>,
    network_id: String,
    blocked_peers: Arc<Mutex<Vec<BlockedPeer>>>,
) -> Vec<SocketAddr> {
    match handle_peer_connection(peer_addr, tls, authority, network_id).await {
        Ok(new_peers) => new_peers,
        Err(_) => {
            debug!("Failed to connect to peer: {}", peer_addr);
            blocked_peers.lock().unwrap().push(BlockedPeer {
                addr: peer_addr,
                expires_at: Instant::now() + Duration::from_secs(PEER_BLOCKLIST_TTL),
            });
            Vec::new()
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
                            info!("Removing unreachable peer: {:?}", addr);
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
