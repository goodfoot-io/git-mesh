// store.rs — fixture source for subsumed_pair parity test
// Shares tokens with cache.rs (cache_entry, entry_size, lookup_key) and
// with evict.rs (store_capacity, evict_policy, expire_time).
// Nothing in all three — pairwise cohesion positive for all pairs.

pub struct StoreRecord {
    pub cache_entry: String,    // shared with cache.rs
    pub entry_size: usize,      // shared with cache.rs
    pub lookup_key: String,     // shared with cache.rs
    pub store_capacity: usize,  // shared with evict.rs
    pub evict_policy: String,   // shared with evict.rs
    pub expire_time: u64,       // shared with evict.rs
    pub store_ref: String,      // unique to store.rs
}

impl StoreRecord {
    pub fn cache_entry_stored(&self) -> bool {
        !self.cache_entry.is_empty()
    }

    pub fn entry_size_within_limit(&self) -> bool {
        self.entry_size <= self.store_capacity
    }

    pub fn lookup_key_valid(&self) -> bool {
        !self.lookup_key.is_empty()
    }

    pub fn store_capacity_check(&self, needed: usize) -> bool {
        self.store_capacity >= needed
    }

    pub fn evict_policy_active(&self) -> bool {
        !self.evict_policy.is_empty()
    }

    pub fn expire_time_valid(&self) -> bool {
        self.expire_time > 0
    }

    pub fn store_summary(&self) -> String {
        format!("key={} size={} capacity={}", self.lookup_key, self.entry_size, self.store_capacity)
    }
}
