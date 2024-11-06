use std::time::Duration;

pub const DNS_RECORD_TTL: u32 = 300;
pub const MAX_CONCURRENT_TASKS: usize = 128;
pub const MAX_RECORDS_TO_RETURN: usize = 32;

pub const PEER_RECHECK_INTERVAL: Duration = Duration::from_secs(300);
pub const PEER_RECHECK_BATCH_SIZE: usize = 1024;

pub const PEER_TIMEOUT: Duration = Duration::from_secs(8);
pub const PEER_REACHABLE_TTL: Duration = Duration::from_secs(3600);
pub const PEER_UNREACHABLE_TTL: Duration = Duration::from_secs(86400);
