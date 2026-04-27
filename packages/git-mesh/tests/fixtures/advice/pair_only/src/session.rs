// session.rs — fixture source file for pair_only parity test
// lines 1-2: preamble
use crate::auth::AuthProvider;

// lines 3-30: session logic (canonical range)
pub struct SessionToken {
    pub session_token: String,
    pub auth_secret: String,
    pub token_expiry: u64,
}

impl SessionToken {
    pub fn validate_session_token(&self, provider: &AuthProvider) -> bool {
        provider.auth_secret_matches(&self.auth_secret)
    }

    pub fn session_token_valid(&self) -> bool {
        !self.session_token.is_empty() && self.token_expiry > 0
    }

    pub fn auth_secret_matches(&self, secret: &str) -> bool {
        self.auth_secret == secret
    }

    pub fn token_expiry_check(&self, now: u64) -> bool {
        now < self.token_expiry
    }

    pub fn auth_session_summary(&self) -> String {
        format!("session={} expiry={}", self.session_token, self.token_expiry)
    }
}
// end of session range
