//! One handshaken Bolt connection: `boltr`'s connection handler behind a pump
//! that sees RESET and a dropped peer while a query runs.
//!
//! Authentication, the nesting and pre-authentication size limits, session
//! cleanup on disconnect, idle-timer refresh and DISCARD all come from `boltr`
//! 0.3.0 itself; `tests/test_bolt_server_guards.py` and
//! `tests/test_bolt_server_discard.py` hold that behaviour with raw Bolt
//! messages no official driver sends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::inflight::InflightCancel;
use crate::pump::{AuthWindow, PumpedReader};
use boltr::error::BoltError;
use boltr::server::connection::Connection;
use boltr::server::{
    AuthInfo, AuthValidator, BoltBackend, RoutingTable, SessionConfig, SessionHandle,
    SessionManager, SessionProperty, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};
use tokio::io::{AsyncRead, AsyncWrite};

/// The server's backend as seen by one connection: every call goes straight to
/// the backend, and the sessions this connection creates are remembered so the
/// pump can cancel the running query of the latest one.
struct SessionTracking<B> {
    inner: Arc<B>,
    sessions: Mutex<Vec<SessionHandle>>,
}

impl<B> SessionTracking<B> {
    fn latest(&self) -> Option<SessionHandle> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last()
            .cloned()
    }
}

#[async_trait::async_trait]
impl<B: BoltBackend> BoltBackend for SessionTracking<B> {
    async fn create_session(&self, config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        let handle = self.inner.create_session(config).await?;
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(handle.clone());
        Ok(handle)
    }

    async fn set_session_auth(
        &self,
        session: &SessionHandle,
        auth_info: AuthInfo,
    ) -> Result<(), BoltError> {
        self.inner.set_session_auth(session, auth_info).await
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        self.inner.close_session(session).await
    }

    async fn configure_session(
        &self,
        session: &SessionHandle,
        property: SessionProperty,
    ) -> Result<(), BoltError> {
        self.inner.configure_session(session, property).await
    }

    async fn reset_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        self.inner.reset_session(session).await
    }

    async fn execute(
        &self,
        session: &SessionHandle,
        query: &str,
        parameters: &HashMap<String, BoltValue>,
        extra: &BoltDict,
        transaction: Option<&TransactionHandle>,
    ) -> Result<boltr::server::ResultStream, BoltError> {
        self.inner
            .execute(session, query, parameters, extra, transaction)
            .await
    }

    async fn begin_transaction(
        &self,
        session: &SessionHandle,
        extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError> {
        self.inner.begin_transaction(session, extra).await
    }

    async fn commit(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        self.inner.commit(session, transaction).await
    }

    async fn rollback(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        self.inner.rollback(session, transaction).await
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        self.inner.get_server_info().await
    }

    async fn route(
        &self,
        routing_context: &BoltDict,
        bookmarks: &[String],
        db: Option<&str>,
    ) -> Result<RoutingTable, BoltError> {
        self.inner.route(routing_context, bookmarks, db).await
    }
}

/// What every connection of a listener shares.
pub struct ConnectionContext<B> {
    pub backend: Arc<B>,
    pub sessions: Arc<SessionManager>,
    pub auth: Option<Arc<dyn AuthValidator>>,
    pub max_message_size: usize,
    /// How long a connection may take from accept to LOGON (and after a
    /// LOGOFF); `None` waits forever.
    pub handshake_timeout: Option<std::time::Duration>,
}

/// Run one handshaken connection.
///
/// The writer is `boltr`'s output half; the reader is the raw input half.
pub async fn serve_connection<R, W, B>(
    reader: R,
    mut writer: W,
    ctx: &ConnectionContext<B>,
    peer_addr: std::net::SocketAddr,
    accepted_at: tokio::time::Instant,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
    B: BoltBackend + InflightCancel + 'static,
{
    let ConnectionContext {
        backend,
        sessions,
        auth,
        max_message_size,
        handshake_timeout,
    } = ctx;
    let tracking = Arc::new(SessionTracking {
        inner: backend.clone(),
        sessions: Mutex::new(Vec::new()),
    });
    // The pump sees RESET and a dropped peer while a query runs, which
    // `boltr` cannot (see `pump.rs`); both cancel the session's running query.
    let interrupt = {
        let tracking = tracking.clone();
        let backend = backend.clone();
        move || {
            if let Some(session) = tracking.latest() {
                backend.cancel_inflight(&session.0);
            }
        }
    };
    let mut reader = PumpedReader::spawn(
        reader,
        interrupt,
        handshake_timeout.map(|repeat| AuthWindow {
            first: accepted_at + repeat,
            repeat,
        }),
    );
    let mut conn = Connection::new(
        &mut reader,
        &mut writer,
        tracking,
        sessions.clone(),
        auth.clone(),
        peer_addr,
        Some(*max_message_size),
    );
    if let Err(e) = conn.run().await {
        tracing::debug!(%peer_addr, error = %e, "Bolt connection closed");
    }
}
