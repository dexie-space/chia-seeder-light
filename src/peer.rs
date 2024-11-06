use crate::config::*;
use crate::dns::*;

use async_channel::{unbounded, Receiver, Sender};
use chia_wallet_sdk::{connect_peer, Connector};
use dashmap::DashSet;
use futures::stream::StreamExt;
use std::{net::SocketAddr, sync::Arc};
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

#[derive(Clone)]
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

    pub async fn process(&self, peer: SocketAddr) {
        if self.processing.insert(peer) {
            if self.sender.try_send(peer).is_err() {
                self.processing.remove(&peer);
            }
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
        receiver
            .for_each_concurrent(MAX_CONCURRENT_TASKS, |peer| {
                let tls = tls.clone();
                let network_id = network_id.clone();
                let sender = sender.clone();
                let processing = processing.clone();
                let authority = authority.clone();

                async move {
                    Self::process_peer(peer, &tls, &authority, &network_id, &sender, &processing)
                        .await;
                }
            })
            .await;
    }

    async fn process_peer(
        peer: SocketAddr,
        tls: &Arc<Connector>,
        authority: &Arc<PeerDiscoveryAuthority>,
        network_id: &Arc<String>,
        sender: &Sender<SocketAddr>,
        processing: &Arc<DashSet<SocketAddr>>,
    ) {
        let previous_peer_status = authority.get_peer_status(&peer).await;

        if matches!(
            previous_peer_status,
            PeerStatus::Unreachable | PeerStatus::Reachable
        ) {
            debug!("Skipping {:?} peer: {:?}", previous_peer_status, peer);
            processing.remove(&peer);
            return;
        }

        let result = timeout(PEER_TIMEOUT, async {
            let (peer_conn, mut stream) =
                connect_peer(network_id.as_str().to_string(), (**tls).clone(), peer).await?;
            let response = peer_conn.request_peers().await?;

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

            if let Err(e) = peer_conn.close().await {
                debug!("Error closing connection for peer {}: {:?}", peer, e);
            }
            drop(peer_conn);

            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(new_peers)
        })
        .await;

        match result {
            Ok(Ok(new_peers)) => {
                authority
                    .mark_peer_reachable(peer, PEER_REACHABLE_TTL, previous_peer_status.clone())
                    .await;
                processing.remove(&peer);

                for new_peer in new_peers {
                    let peer_status = authority.get_peer_status(&new_peer).await;

                    // ignore already processed peers
                    if matches!(peer_status, PeerStatus::Unreachable | PeerStatus::Reachable) {
                        continue;
                    }

                    // Process new peers received from the peer
                    if processing.insert(new_peer) {
                        if sender.try_send(new_peer).is_err() {
                            processing.remove(&new_peer);
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                debug!("Error for peer {}: {:?}", peer, e);
                authority
                    .mark_peer_unreachable(peer, PEER_UNREACHABLE_TTL, previous_peer_status)
                    .await;
                processing.remove(&peer);
            }
            Err(_) => {
                debug!("Timeout for peer {}", peer);
                authority
                    .mark_peer_unreachable(peer, PEER_UNREACHABLE_TTL, previous_peer_status)
                    .await;
                processing.remove(&peer);
            }
        }
    }
}

pub async fn start_peer_rechecker(
    processor: PeerProcessor,
    authority: Arc<PeerDiscoveryAuthority>,
) -> anyhow::Result<()> {
    loop {
        authority.cleanup_unreachable_peers();

        let expired_peers = authority
            .get_expired_reachable_peers(PEER_RECHECK_BATCH_SIZE)
            .await;

        let reachable_peer_count = authority.get_reachable_peer_count();

        info!(
            "Starting periodic peer recheck, {} reachable peers, {} processing",
            reachable_peer_count,
            processor.processing.len()
        );

        for peer in expired_peers {
            processor.process(peer).await;
        }

        sleep(PEER_RECHECK_INTERVAL).await;
    }
}
