use crate::config::*;
use crate::dns::*;

use async_channel::{unbounded, Receiver, Sender};
use chia_wallet_sdk::{connect_peer, Connector};
use dashmap::DashSet;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

pub struct PeerProcessor {
    sender: Sender<SocketAddr>,
    processing: Arc<DashSet<SocketAddr>>,
}

impl PeerProcessor {
    pub fn new(
        tls: Connector,
        authority: Arc<RandomizedAuthority>,
        network_id: String,
        is_recheck: bool,
    ) -> Self {
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
            is_recheck,
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
        authority: Arc<RandomizedAuthority>,
        network_id: Arc<String>,
        processing: Arc<DashSet<SocketAddr>>,
        sender: Sender<SocketAddr>,
        is_recheck: bool,
    ) {
        let mut tasks = FuturesUnordered::new();

        loop {
            tokio::select! {
                Some(_) = tasks.next() => {},
                recv_result = receiver.recv() => {
                    match recv_result {
                        Ok(peer) => {
                            if tasks.len() < MAX_CONCURRENT_TASKS {
                                tasks.push(Self::process_peer(
                                    peer,
                                    tls.clone(),
                                    authority.clone(),
                                    network_id.clone(),
                                    sender.clone(),
                                    is_recheck,
                                    processing.clone(),
                                ));
                            } else {
                                // Wait for a task to complete before processing more peers
                                tasks.next().await;
                            }
                        }
                        Err(_) => {
                            // Receiver is closed; if no tasks are left, break the loop
                            if tasks.is_empty() {
                                break;
                            }
                        }
                    }
                },
            }
        }
    }

    async fn process_peer(
        peer: SocketAddr,
        tls: Arc<Connector>,
        authority: Arc<RandomizedAuthority>,
        network_id: Arc<String>,
        sender: Sender<SocketAddr>,
        is_recheck: bool,
        processing: Arc<DashSet<SocketAddr>>,
    ) {
        if !is_recheck && authority.known_peer(&peer).await {
            processing.remove(&peer);
            return;
        }

        if authority.is_blocked(&peer) {
            debug!("Skipping blocked peer: {:?}", peer);
            processing.remove(&peer);
            return;
        }

        let result = timeout(Duration::from_secs(PEER_TIMEOUT), async {
            let (peer_conn, mut stream) =
                connect_peer((*network_id).clone(), (*tls).clone(), peer).await?;
            let response = peer_conn.request_peers().await?;

            authority.add_peer(peer_conn.socket_addr()).await;

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
                    .block_peer(peer, Duration::from_secs(PEER_BLOCKLIST_TTL))
                    .await;
            }
            Err(_) => {
                debug!("Timeout for peer {}", peer);
                authority
                    .block_peer(peer, Duration::from_secs(PEER_BLOCKLIST_TTL))
                    .await;
            }
        }
    }
}

pub fn start_peer_crawler(
    initial_peers: Vec<SocketAddr>,
    tls: Connector,
    authority: Arc<RandomizedAuthority>,
    network_id: String,
) -> tokio::task::JoinHandle<()> {
    let processor = PeerProcessor::new(tls, authority, network_id, false);

    tokio::spawn(async move {
        for peer in initial_peers {
            processor.process(peer);
        }
    })
}

pub async fn start_peer_rechecker(
    tls: Connector,
    authority: Arc<RandomizedAuthority>,
    network_id: String,
) -> anyhow::Result<()> {
    let processor = PeerProcessor::new(tls, authority.clone(), network_id, true);

    loop {
        sleep(RECHECK_INTERVAL).await;

        let peers = authority.get_peers().await;
        let peers_len = peers.len();
        let processing_len = processor.processing.len();

        info!(
            "Starting periodic peer recheck, {} reachable peers, {} processing",
            peers_len, processing_len
        );

        authority.cleanup_blocklist();

        if processing_len < peers_len {
            for peer in peers {
                processor.process(peer);
            }
        }
    }
}
