use crate::config::*;

use rand::seq::SliceRandom;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::{sync::Mutex, time::Instant};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::RwLock;
use tracing::info;
use trust_dns_server::authority::Catalog;
use trust_dns_server::authority::{
    AnyRecords, AuthLookup, Authority, LookupError, LookupOptions, LookupRecords, MessageRequest,
    ZoneType,
};
use trust_dns_server::proto::op::ResponseCode;
use trust_dns_server::proto::rr::rdata::{A, AAAA};
use trust_dns_server::proto::rr::RecordSet;
use trust_dns_server::proto::rr::{LowerName, Name, RData, Record, RecordType};
use trust_dns_server::server::RequestInfo;
use trust_dns_server::ServerFuture;

struct BlockedPeer {
    addr: SocketAddr,
    expires_at: Instant,
}

pub struct RandomizedAuthority {
    peers: Arc<RwLock<HashSet<SocketAddr>>>,
    origin: LowerName,
    blocked_peers: Arc<Mutex<Vec<BlockedPeer>>>,
}

impl RandomizedAuthority {
    pub fn new(origin: Name) -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashSet::new())),
            origin: LowerName::new(&origin),
            blocked_peers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn add_peer(&self, addr: SocketAddr) {
        let mut peers = self.peers.write().await;
        if peers.insert(addr) {
            info!("Added reachable peer: {:?}", addr);
        }
    }

    pub async fn remove_peer(&self, addr: SocketAddr) {
        let mut peers = self.peers.write().await;
        if peers.remove(&addr) {
            info!("Removed unreachable peer: {:?}", addr);
        }
    }

    pub async fn known_peer(&self, addr: &SocketAddr) -> bool {
        self.peers.read().await.contains(addr)
    }

    pub async fn get_peers(&self) -> Vec<SocketAddr> {
        let peers = self.peers.read().await;
        peers.iter().cloned().collect()
    }

    pub async fn block_peer(&self, addr: SocketAddr, duration: Duration) {
        self.remove_peer(addr).await;

        let mut blocked = self.blocked_peers.lock().unwrap();
        blocked.retain(|peer| peer.addr != addr);
        blocked.push(BlockedPeer {
            addr,
            expires_at: Instant::now() + duration,
        });
    }

    pub fn is_blocked(&self, addr: &SocketAddr) -> bool {
        let blocked = self.blocked_peers.lock().unwrap();
        let now = Instant::now();
        blocked
            .iter()
            .any(|peer| &peer.addr == addr && peer.expires_at > now)
    }

    pub fn cleanup_blocklist(&self) -> () {
        let mut blocked = self.blocked_peers.lock().unwrap();
        blocked.retain(|peer| peer.expires_at > Instant::now());
    }
}

#[async_trait::async_trait]
impl Authority for RandomizedAuthority {
    type Lookup = AuthLookup;

    fn zone_type(&self) -> ZoneType {
        ZoneType::Primary
    }

    fn is_axfr_allowed(&self) -> bool {
        false
    }

    fn origin(&self) -> &LowerName {
        &self.origin
    }

    async fn lookup(
        &self,
        name: &LowerName,
        rtype: RecordType,
        options: LookupOptions,
    ) -> Result<Self::Lookup, LookupError> {
        if name != &self.origin {
            return Ok(AuthLookup::default());
        }

        let peers = self.peers.read().await;
        let mut filtered_peers: Vec<_> = match rtype {
            RecordType::A => peers
                .iter()
                .filter(|&&addr| matches!(addr.ip(), std::net::IpAddr::V4(_)))
                .collect(),
            RecordType::AAAA => peers
                .iter()
                .filter(|&&addr| matches!(addr.ip(), std::net::IpAddr::V6(_)))
                .collect(),
            _ => Vec::new(), // empty for unsupported record types
        };

        // Shuffle the filtered peers
        let mut rng = rand::thread_rng();
        filtered_peers.shuffle(&mut rng);

        let mut records = Vec::new();

        for &addr in filtered_peers.iter().take(MAX_RECORDS_TO_RETURN) {
            let record = match addr.ip() {
                std::net::IpAddr::V4(ipv4) if matches!(rtype, RecordType::A | RecordType::ANY) => {
                    Some(Record::from_rdata(
                        self.origin.clone().into(),
                        DNS_RECORD_TTL,
                        RData::A(A(ipv4)),
                    ))
                }
                std::net::IpAddr::V6(ipv6)
                    if matches!(rtype, RecordType::AAAA | RecordType::ANY) =>
                {
                    Some(Record::from_rdata(
                        self.origin.clone().into(),
                        DNS_RECORD_TTL,
                        RData::AAAA(AAAA(ipv6)),
                    ))
                }
                _ => None,
            };

            if let Some(record) = record {
                records.push(record);
            }
        }

        if records.is_empty() {
            return Ok(AuthLookup::default());
        }

        let mut record_set = RecordSet::new(&self.origin.clone().into(), rtype, DNS_RECORD_TTL);
        for record in records {
            record_set.insert(record, 0);
        }
        let records_vec = vec![Arc::new(record_set)];

        Ok(AuthLookup::answers(
            LookupRecords::AnyRecords(AnyRecords::new(
                options,
                records_vec,
                rtype,
                self.origin.clone(),
            )),
            None,
        ))
    }

    async fn search(
        &self,
        request: RequestInfo<'_>,
        options: LookupOptions,
    ) -> Result<Self::Lookup, LookupError> {
        Authority::lookup(
            self,
            request.query.name(),
            request.query.query_type(),
            options,
        )
        .await
    }

    async fn get_nsec_records(
        &self,
        _name: &LowerName,
        _options: LookupOptions,
    ) -> Result<Self::Lookup, LookupError> {
        Ok(AuthLookup::default())
    }

    async fn update(&self, _update: &MessageRequest) -> Result<bool, ResponseCode> {
        Ok(false)
    }
}

pub async fn start_dns_server(
    catalog: Catalog,
    listen_address: SocketAddr,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let mut server = ServerFuture::new(catalog);
    let udp_socket = UdpSocket::bind(listen_address).await?;
    let tcp_listener = TcpListener::bind(listen_address).await?;

    info!("Starting DNS server on {}", listen_address);

    Ok(tokio::spawn(async move {
        server.register_socket(udp_socket);
        server.register_listener(tcp_listener, Duration::from_secs(PEER_TIMEOUT));
        server.block_until_done().await.unwrap();
    }))
}
