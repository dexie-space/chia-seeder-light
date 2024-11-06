use std::time::Duration;

pub const MAX_CONCURRENT_CONNECTIONS: usize = 128; // Maximum number of concurrent outgoing peer connections

pub const DNS_RECORD_TTL: u32 = 300; // TTL for DNS records
pub const MAX_RECORDS_TO_RETURN: usize = 32; // Maximum number of records to return in DNS responses

pub const PEER_RECHECK_INTERVAL: Duration = Duration::from_secs(300); // Runs periodically to push expired peers back into the queue
pub const PEER_RECHECK_BATCH_SIZE: usize = 1024; // Batch size for rechecking expired peers

pub const PEER_TIMEOUT: Duration = Duration::from_secs(8); // The timeout for each peer (includes connection, handshake, and request-peers response)
pub const PEER_REACHABLE_TTL: Duration = Duration::from_secs(3600); // How long until a reachable peer is pushed back into the recheck queue
pub const PEER_UNREACHABLE_TTL: Duration = Duration::from_secs(86400); // Duration that must pass before an unreachable peer can be considered for connection again
