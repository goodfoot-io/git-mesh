// auth.rs — fixture source file for pair_only parity test
// lines 1-4: preamble
use crate::session::SessionToken;
pub mod auth_core;

// lines 5-40: auth logic (canonical anchor)
pub struct AuthProvider {
    pub session_token: String,
    pub auth_secret: String,
    pub token_expiry: u64,
}

impl AuthProvider {
    pub fn validate_session_token(&self, token: &SessionToken) -> bool {
        token.auth_secret == self.auth_secret
    }

    pub fn session_token_valid(&self) -> bool {
        !self.session_token.is_empty()
    }

    pub fn auth_secret_matches(&self, secret: &str) -> bool {
        self.auth_secret == secret
    }

    pub fn token_expiry_check(&self, now: u64) -> bool {
        now < self.token_expiry
    }

    pub fn renew_session_token(&mut self, token: &SessionToken) {
        self.session_token = token.session_token.clone();
    }

    pub fn auth_session_summary(&self) -> String {
        format!("token={} expiry={}", self.session_token, self.token_expiry)
    }
}
// end of auth anchor
