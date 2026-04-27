// evict.rs — fixture source for subsumed_pair parity test
// Shares tokens with cache.rs (cache_ttl, expiry_check, evict_count) and
// with store.rs (store_capacity, evict_policy, expire_time).
// Nothing in all three — pairwise cohesion positive for all pairs.

pub struct EvictRecord {
    pub cache_ttl: u64,          // shared with cache.rs
    pub expiry_check: bool,      // shared with cache.rs
    pub evict_count: u32,        // shared with cache.rs
    pub store_capacity: usize,   // shared with store.rs
    pub evict_policy: String,    // shared with store.rs
    pub expire_time: u64,        // shared with store.rs
    pub evict_ref: String,       // unique to evict.rs
}

impl EvictRecord {
    pub fn cache_ttl_expired(&self, now: u64) -> bool {
        now > self.cache_ttl
    }

    pub fn expiry_check_run(&self) -> bool {
        self.expiry_check
    }

    pub fn evict_count_limit(&self, limit: u32) -> bool {
        self.evict_count < limit
    }

    pub fn store_capacity_available(&self, needed: usize) -> bool {
        self.store_capacity >= needed
    }

    pub fn evict_policy_valid(&self) -> bool {
        !self.evict_policy.is_empty()
    }

    pub fn expire_time_check(&self, now: u64) -> bool {
        now > self.expire_time
    }

    pub fn evict_summary(&self) -> String {
        format!("policy={} ttl={} capacity={}", self.evict_policy, self.cache_ttl, self.store_capacity)
    }
}
