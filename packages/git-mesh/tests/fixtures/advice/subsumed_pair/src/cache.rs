// cache.rs — fixture source for subsumed_pair parity test
// Shares tokens with store.rs (cache_entry, entry_size, lookup_key) and
// with evict.rs (cache_ttl, expiry_check, evict_count).
// Nothing in all three — pairwise cohesion positive for all pairs.

pub struct CacheRecord {
    pub cache_entry: String,    // shared with store.rs
    pub entry_size: usize,      // shared with store.rs
    pub lookup_key: String,     // shared with store.rs
    pub cache_ttl: u64,         // shared with evict.rs
    pub expiry_check: bool,     // shared with evict.rs
    pub evict_count: u32,       // shared with evict.rs
    pub cache_ref: String,      // unique to cache.rs
}

impl CacheRecord {
    pub fn cache_entry_valid(&self) -> bool {
        !self.cache_entry.is_empty()
    }

    pub fn entry_size_check(&self, limit: usize) -> bool {
        self.entry_size <= limit
    }

    pub fn lookup_key_set(&self) -> bool {
        !self.lookup_key.is_empty()
    }

    pub fn cache_ttl_valid(&self) -> bool {
        self.cache_ttl > 0
    }

    pub fn expiry_check_result(&self) -> bool {
        self.expiry_check
    }

    pub fn evict_count_exceeded(&self, limit: u32) -> bool {
        self.evict_count >= limit
    }

    pub fn cache_summary(&self) -> String {
        format!("key={} size={} ttl={}", self.lookup_key, self.entry_size, self.cache_ttl)
    }
}
