//! The Bolt accept loop, owned here instead of delegated to `boltr`'s
//! `BoltServer::serve`.
//!
//! `boltr` 0.2 binds its own listener and never sets `TCP_NODELAY` on the
//! streams it accepts, and exposes no hook to do so. Bolt is request/response
//! with several small messages per transaction, so on Linux every small reply
//! waits in Nagle's buffer for the client's delayed ACK (~44 ms per exchange,
//! ~132 ms per explicit transaction; ~1 ms / ~2 ms with the option set).
//!
//! Everything a connection needs after `accept` is public in `boltr`, so this
//! module re-creates only the listener side: bind, session reaper, accept loop,
//! shutdown, optional TLS. Per-connection work is `boltr`'s own
//! `server_handshake` + `Connection::run`, unchanged.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::coalesce::CoalescingWriter;
use boltr::error::BoltError;
use boltr::server::connection::Connection;
use boltr::server::handshake::server_handshake;
use boltr::server::{AuthValidator, BoltBackend, SessionHandle, SessionManager};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

/// What the accept loop needs; the counterpart of `boltr::server::BoltServer`'s
/// builder fields.
pub struct BoltListener<B: BoltBackend> {
    pub backend: B,
    pub auth: Option<Arc<dyn AuthValidator>>,
    pub tls: Option<TlsAcceptor>,
    pub idle_timeout: Option<Duration>,
    pub max_sessions: usize,
    pub max_message_size: usize,
    pub shutdown: Pin<Box<dyn Future<Output = ()> + Send>>,
}

fn invalid_tls<E: Into<Box<dyn std::error::Error + Send + Sync>>>(e: E) -> BoltError {
    BoltError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Build a TLS acceptor from PEM cert chain and key bytes.
///
/// Needs a process-wide rustls crypto provider to be installed first.
pub fn tls_acceptor_from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<TlsAcceptor, BoltError> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(invalid_tls)?;
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(invalid_tls)?;
    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(invalid_tls)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Per-socket setup, applied to the raw TCP stream before any TLS handshake so
/// plain and TLS connections both get it. A failure is logged, not fatal: the
/// connection still works, only slower.
fn tune_stream(stream: &TcpStream, peer_addr: SocketAddr) {
    if let Err(e) = stream.set_nodelay(true) {
        tracing::debug!(%peer_addr, error = %e, "could not set TCP_NODELAY");
    }
}

impl<B: BoltBackend> BoltListener<B> {
    /// Bind `addr` and serve until the shutdown future resolves.
    ///
    /// Like `boltr`'s loop, in-flight connection tasks are detached and are
    /// not awaited at shutdown.
    pub async fn serve(self, addr: SocketAddr) -> Result<(), BoltError> {
        let listener = TcpListener::bind(addr).await?;
        let backend = Arc::new(self.backend);
        let sessions = Arc::new(SessionManager::new(Some(self.max_sessions)));

        let reaper = self.idle_timeout.map(|timeout| {
            let sessions = sessions.clone();
            let backend = backend.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(timeout / 2);
                loop {
                    interval.tick().await;
                    for id in sessions.reap_idle(timeout) {
                        let _ = backend.close_session(&SessionHandle(id.clone())).await;
                        tracing::debug!(session_id = %id, "reaped idle Bolt session");
                    }
                }
            })
        });

        let tls_label = if self.tls.is_some() { " (TLS)" } else { "" };
        tracing::info!(%addr, "Bolt server listening{}", tls_label);

        let tls = self.tls.map(Arc::new);
        let mut shutdown = self.shutdown;
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer_addr)) => {
                        tune_stream(&stream, peer_addr);
                        tokio::spawn(run_connection(
                            stream,
                            peer_addr,
                            backend.clone(),
                            sessions.clone(),
                            self.auth.clone(),
                            tls.clone(),
                            self.max_message_size,
                        ));
                    }
                    Err(e) => tracing::warn!(error = %e, "accept error"),
                },
                () = &mut shutdown => {
                    tracing::info!("Bolt server shutting down");
                    break;
                }
            }
        }

        if let Some(handle) = reaper {
            handle.abort();
        }
        Ok(())
    }
}

async fn run_connection<B: BoltBackend>(
    stream: TcpStream,
    peer_addr: SocketAddr,
    backend: Arc<B>,
    sessions: Arc<SessionManager>,
    auth: Option<Arc<dyn AuthValidator>>,
    tls: Option<Arc<TlsAcceptor>>,
    max_message_size: usize,
) {
    match tls {
        Some(acceptor) => match acceptor.accept(stream).await {
            Ok(stream) => {
                handshake_and_run(stream, peer_addr, backend, sessions, auth, max_message_size)
                    .await
            }
            Err(e) => tracing::debug!(%peer_addr, error = %e, "TLS handshake failed"),
        },
        None => {
            handshake_and_run(stream, peer_addr, backend, sessions, auth, max_message_size).await
        }
    }
}

async fn handshake_and_run<S, B>(
    mut stream: S,
    peer_addr: SocketAddr,
    backend: Arc<B>,
    sessions: Arc<SessionManager>,
    auth: Option<Arc<dyn AuthValidator>>,
    max_message_size: usize,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    B: BoltBackend,
{
    match server_handshake(&mut stream).await {
        Ok(version) => {
            tracing::debug!(%peer_addr, ?version, "Bolt handshake complete");
            let (reader, writer) = tokio::io::split(stream);
            let mut conn = Connection::new(
                reader,
                CoalescingWriter::new(writer),
                backend,
                sessions,
                auth,
                peer_addr,
                Some(max_message_size),
            );
            if let Err(e) = conn.run().await {
                tracing::debug!(%peer_addr, error = %e, "Bolt connection closed");
            }
        }
        Err(e) => tracing::debug!(%peer_addr, error = %e, "Bolt handshake failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A freshly accepted socket has Nagle on; `tune_stream` is what turns it
    /// off, for every connection the loop accepts (issue #201).
    #[tokio::test]
    async fn accepted_streams_get_tcp_nodelay() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _client = TcpStream::connect(addr).await.unwrap();
        let (server_side, peer) = listener.accept().await.unwrap();
        assert!(
            !server_side.nodelay().unwrap(),
            "premise: Nagle is on by default"
        );
        tune_stream(&server_side, peer);
        assert!(server_side.nodelay().unwrap());
    }
}
