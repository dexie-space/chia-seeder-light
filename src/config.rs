use std::time::Duration;

pub const PEER_TIMEOUT: u64 = 7;
pub const DNS_RECORD_TTL: u32 = 300;
pub const MAX_CONCURRENT_TASKS: usize = 100;
pub const MAX_RECORDS_TO_RETURN: usize = 32;
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(1800);
pub const PEER_BLOCKLIST_TTL: u64 = 86400;
