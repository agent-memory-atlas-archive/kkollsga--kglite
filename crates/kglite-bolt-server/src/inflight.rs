//! The query each session is running right now, so the transport can cancel it.
//!
//! `boltr` runs a connection's messages one at a time and does not read the
//! socket while a RUN executes, so the backend never sees a RESET or a dropped
//! connection that arrives mid-query. The transport (`pump.rs`) does, and calls
//! [`InflightCancel::cancel_inflight`] with the session id; this registry maps
//! that id to the running query's [`CancelToken`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kglite::api::session::CancelToken;

/// Cancel whatever query a session is running. A no-op when it runs none.
pub trait InflightCancel {
    fn cancel_inflight(&self, session_id: &str);
}

/// Session id to the token of its running query. A session runs one query at a
/// time (the connection is sequential), so one slot per session suffices.
#[derive(Default)]
pub struct InflightQueries {
    tokens: Arc<Mutex<HashMap<String, CancelToken>>>,
}

impl InflightQueries {
    /// Register a fresh token for `session_id`. The returned guard owns the
    /// caller's clone and must stay alive until the query returns: the token's
    /// flag slot is recycled when the last clone drops.
    pub fn begin(&self, session_id: &str) -> InflightGuard {
        let token = CancelToken::new();
        self.tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session_id.to_string(), token.clone());
        InflightGuard {
            token,
            session_id: session_id.to_string(),
            tokens: Arc::clone(&self.tokens),
        }
    }

    pub fn cancel(&self, session_id: &str) {
        let token = self
            .tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
        }
    }
}

/// Deregisters the session's token on drop.
pub struct InflightGuard {
    token: CancelToken,
    session_id: String,
    tokens: Arc<Mutex<HashMap<String, CancelToken>>>,
}

impl InflightGuard {
    pub fn token(&self) -> &CancelToken {
        &self.token
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_reaches_the_running_query_only() {
        let q = InflightQueries::default();
        let a = q.begin("s1");
        let b = q.begin("s2");
        q.cancel("s1");
        assert!(a.token().is_cancelled());
        assert!(!b.token().is_cancelled());
        drop(a);
        q.cancel("s1");
        q.cancel("nobody");
        assert!(!b.token().is_cancelled());
    }
}
