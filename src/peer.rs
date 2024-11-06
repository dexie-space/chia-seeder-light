use crate::config::*;
use crate::dns::*;

use async_channel::{unbounded, Receiver, Sender};
use chia_wallet_sdk::{connect_peer, Connector};
use dashmap::DashSet;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::{net::SocketAddr, sync::Arc};
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

pub struct PeerProcessor {
    sender: Sender<SocketAddr>,
    processing: Arc<DashSet<SocketAddr>>,
}

impl PeerProcessor {
    pub fn new(tls: Connector, authority: Arc<PeerDiscoveryAuthority>, network_id: String) -> Self {
        let (sender, receiver) = unbounded();
        let processing = Arc::new(DashSet::new());

        let tls = Arc::new(tls);
        let network_id = Arc::new(network_id);

        tokio::spawn(Self::process_peers(
            receiver,
            tls.clone(),
            authority.clone(),
            network_id.clone(),
            processing.clone(),
            sender.clone(),
        ));

        Self { sender, processing }
    }

    pub fn process(&self, peer: SocketAddr) {
        // Only send if not already processing this peer
        if self.processing.insert(peer) {
            let _ = self.sender.try_send(peer);
        }
    }

    async fn process_peers(
        receiver: Receiver<SocketAddr>,
        tls: Arc<Connector>,
        authority: Arc<PeerDiscoveryAuthority>,
        network_id: Arc<String>,
        processing: Arc<DashSet<SocketAddr>>,
        sender: Sender<SocketAddr>,
    ) {
        let mut tasks = FuturesUnordered::new();

        loop {
            tokio::select! {
                Some(_) = tasks.next() => {},
                recv_result = receiver.recv() => {
                    match recv_result {
                        Ok(peer) => {
                            let previous_peer_status = authority.get_peer_status(&peer).await;

                            match previous_peer_status {
                                PeerStatus::Unreachable => {
                                    debug!("Skipping unreachable peer: {:?}", peer);
                                    continue;
                                }
                                PeerStatus::Reachable => {
                                    debug!("Skipping reachable peer: {:?}", peer);
                                    continue;
                                }
                                _ => {}
                            }

                            if tasks.len() < MAX_CONCURRENT_TASKS {
                                tasks.push(Self::process_peer(
                                    peer,
                                    tls.clone(),
                                    authority.clone(),
                                    network_id.clone(),
                                    sender.clone(),
                                    processing.clone(),
                                    previous_peer_status,
                                ));
                            } else {
                                // Wait for a task to complete before processing more peers
                                tasks.next().await;
                            }
                        }
                        Err(_) => {
                            // Receiver is closed; wait for remaining tasks to complete
                            while !tasks.is_empty() {
                                tasks.next().await;
                            }
                            break;
                        }
                    }
                },
            }
        }
    }

    async fn process_peer(
        peer: SocketAddr,
        tls: Arc<Connector>,
        authority: Arc<PeerDiscoveryAuthority>,
        network_id: Arc<String>,
        sender: Sender<SocketAddr>,
        processing: Arc<DashSet<SocketAddr>>,
        previous_peer_status: PeerStatus,
    ) {
        let result = timeout(PEER_TIMEOUT, async {
            let (peer_conn, mut stream) =
                connect_peer((*network_id).clone(), (*tls).clone(), peer).await?;
            let response = peer_conn.request_peers().await?;

            authority
                .mark_peer_reachable(
                    peer_conn.socket_addr(),
                    PEER_REACHABLE_TTL,
                    previous_peer_status.clone(),
                )
                .await;

            let new_peers = response
                .peer_list
                .into_iter()
                .filter_map(|peer_info| {
                    peer_info
                        .host
                        .parse()
                        .ok()
                        .map(|ip| SocketAddr::new(ip, peer_info.port))
                })
                .collect::<Vec<_>>();

            // Ensure resources close before completing
            stream.close();
            drop(stream);

            match peer_conn.close().await {
                Ok(_) => (),
                Err(e) => debug!("Error closing connection for peer {}: {:?}", peer, e),
            }
            drop(peer_conn);

            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(new_peers)
        })
        .await;

        processing.remove(&peer);

        match result {
            Ok(Ok(new_peers)) => {
                for new_peer in new_peers {
                    let _ = sender.try_send(new_peer);
                }
            }
            Ok(Err(e)) => {
                debug!("Error for peer {}: {:?}", peer, e);
                authority
                    .mark_peer_unreachable(peer, PEER_UNREACHABLE_TTL, previous_peer_status.clone())
                    .await;
            }
            Err(_) => {
                debug!("Timeout for peer {}", peer);
                authority
                    .mark_peer_unreachable(peer, PEER_UNREACHABLE_TTL, previous_peer_status)
                    .await;
            }
        }
    }
}

pub fn start_peer_crawler(
    initial_peers: Vec<SocketAddr>,
    tls: Connector,
    authority: Arc<PeerDiscoveryAuthority>,
    network_id: String,
) -> tokio::task::JoinHandle<()> {
    let processor = PeerProcessor::new(tls, authority, network_id);

    tokio::spawn(async move {
        for peer in initial_peers {
            processor.process(peer);
        }
    })
}

pub async fn start_peer_rechecker(
    tls: Connector,
    authority: Arc<PeerDiscoveryAuthority>,
    network_id: String,
) -> anyhow::Result<()> {
    let processor = PeerProcessor::new(tls, authority.clone(), network_id);

    loop {
        authority.cleanup_unreachable_peers();

        let peers = authority
            .get_peers(true, PEER_RECHECK_BATCH_SIZE, None)
            .await;

        let peers_len = peers.len();
        let processing_len = processor.processing.len();
        let reachable_peer_count = authority.get_reachable_peer_count();

        info!(
            "Starting periodic peer recheck, {} reachable peers ({} expired), {} processing",
            reachable_peer_count, peers_len, processing_len
        );

        if processing_len < peers_len {
            for peer in peers.iter().take(peers_len - processing_len) {
                processor.process(*peer);
            }
        }

        sleep(PEER_RECHECK_INTERVAL).await;
    }
}
