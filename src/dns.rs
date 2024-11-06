use crate::config::*;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};
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

#[derive(Debug, PartialEq, Clone)]
pub enum PeerStatus {
    Reachable,
    Unreachable,
    ReachableExpired,
    UnreachableExpired,
    Unknown,
}

pub struct PeerDiscoveryAuthority {
    origin: LowerName,
    db_pool: Pool<SqliteConnectionManager>,
}

impl PeerDiscoveryAuthority {
    pub fn new(origin: Name) -> Self {
        let db_path = PathBuf::from("peers.db");
        let manager = SqliteConnectionManager::file(&db_path);
        let pool = Pool::new(manager).unwrap();

        // Initialize the database
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS peers (
                addr TEXT PRIMARY KEY,
                is_reachable INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();

        Self {
            origin: LowerName::new(&origin),
            db_pool: pool,
        }
    }

    pub async fn get_peers(&self, max_records: usize, ip_version: Option<&str>) -> Vec<SocketAddr> {
        let conn = self.db_pool.get().unwrap();

        let ip_filter = match ip_version {
            Some("v4") => "AND addr NOT LIKE '%:%:%'",
            Some("v6") => "AND addr LIKE '%:%:%'",
            _ => "",
        };

        let query = format!(
            "SELECT addr FROM peers WHERE is_reachable = 1 {} ORDER BY RANDOM() LIMIT ?",
            ip_filter
        );

        let mut stmt = conn.prepare(&query).unwrap();

        stmt.query_map([&max_records.to_string()], |row| {
            Ok(row.get::<_, String>(0)?.parse().ok())
        })
        .unwrap()
        .filter_map(Result::ok)
        .flatten()
        .collect()
    }

    pub async fn get_expired_reachable_peers(&self, max_records: usize) -> Vec<SocketAddr> {
        let conn = self.db_pool.get().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT addr FROM peers
                WHERE is_reachable = 1 AND expires_at <= strftime('%s', 'now')
                ORDER BY RANDOM() LIMIT ?",
            )
            .unwrap();

        let peers = stmt
            .query_map([&max_records.to_string()], |row| {
                let addr_str: String = row.get(0)?;
                Ok(addr_str.parse::<SocketAddr>().ok())
            })
            .unwrap()
            .filter_map(Result::ok)
            .flatten()
            .collect();

        peers
    }

    pub async fn mark_peer_reachable(
        &self,
        addr: SocketAddr,
        duration: Duration,
        previous_peer_status: PeerStatus,
    ) {
        let conn = self.db_pool.get().unwrap();
        let duration_seconds = duration.as_secs();

        conn.execute(
            "INSERT OR REPLACE INTO peers (addr, is_reachable, expires_at)
             VALUES (?1, 1, strftime('%s', 'now', ?2 || ' seconds'))",
            [&addr.to_string(), &duration_seconds.to_string()],
        )
        .unwrap();

        if previous_peer_status == PeerStatus::UnreachableExpired
            || previous_peer_status == PeerStatus::Unknown
        {
            info!("Added reachable peer: {:?}", addr);
        }
    }

    pub async fn mark_peer_unreachable(
        &self,
        addr: SocketAddr,
        duration: Duration,
        previous_peer_status: PeerStatus,
    ) {
        let conn = self.db_pool.get().unwrap();
        let duration_seconds = duration.as_secs();

        conn.execute(
            "INSERT OR REPLACE INTO peers (addr, is_reachable, expires_at)
             VALUES (?1, 0, strftime('%s', 'now', ?2 || ' seconds'))",
            [&addr.to_string(), &duration_seconds.to_string()],
        )
        .unwrap();

        if previous_peer_status == PeerStatus::ReachableExpired {
            info!("Removed unreachable peer: {:?}", addr);
        }
    }

    pub async fn get_peer_status(&self, addr: &SocketAddr) -> PeerStatus {
        let conn = self.db_pool.get().unwrap();

        let result: Result<(i64, i64), _> = conn.query_row(
            "SELECT is_reachable, CASE WHEN expires_at <= strftime('%s', 'now') THEN 1 ELSE 0 END AS is_expired
             FROM peers WHERE addr = ?1",
            [&addr.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );

        match result {
            Ok((is_reachable, is_expired)) => match (is_reachable, is_expired) {
                (1, 0) => PeerStatus::Reachable,
                (0, 0) => PeerStatus::Unreachable,
                (1, 1) => PeerStatus::ReachableExpired,
                (0, 1) => PeerStatus::UnreachableExpired,
                _ => PeerStatus::Unknown,
            },
            Err(_) => PeerStatus::Unknown,
        }
    }

    pub fn cleanup_unreachable_peers(&self) {
        let conn = self.db_pool.get().unwrap();

        conn.execute(
            "DELETE FROM peers WHERE is_reachable = 0 AND expires_at <= strftime('%s', 'now')",
            [],
        )
        .unwrap();
    }

    pub fn get_reachable_peer_count(&self) -> usize {
        let conn = self.db_pool.get().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM peers WHERE is_reachable = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        count as usize
    }
}

#[async_trait::async_trait]
impl Authority for PeerDiscoveryAuthority {
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

        let ip_version = match rtype {
            RecordType::A => Some("v4"),
            RecordType::AAAA => Some("v6"),
            _ => None,
        };

        let peers = self.get_peers(MAX_RECORDS_TO_RETURN, ip_version).await;

        let mut records = Vec::new();

        for addr in peers {
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
        server.register_listener(tcp_listener, PEER_TIMEOUT);
        server.block_until_done().await.unwrap();
    }))
}
