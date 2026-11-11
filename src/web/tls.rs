//! HTTPS and plain HTTP on one port (SPECS §33, a worker serving the SPA).
//!
//! A worker serves two kinds of client on `[worker] port`: FlightDeck Desktop,
//! which dials `ws://` and must keep doing so, and a browser, which needs
//! `https://` with a real certificate before it will treat the page as a
//! secure context. Rather than a second port (which every paired Desktop and
//! every firewall rule would have to learn), each connection is **sniffed**:
//! a TLS connection's first byte is always `0x16` (a handshake record), and a
//! plain HTTP request's is an ASCII method letter, so one peeked byte tells
//! them apart without consuming anything.
//!
//! ## Why the sniffing is not in `Listener::accept`
//!
//! axum 0.8 calls [`axum::serve::Listener::accept`] in a loop and awaits each
//! call before the next: anything slow inside it (a peek that waits for a
//! client to speak, a TLS handshake) would stall every other connection
//! behind one slow or silent client. So [`sniff`] runs its own accept loop,
//! gives each connection a task of its own to peek and handshake, and hands
//! the ready streams to [`SniffListener`] over a channel; axum's `accept` only
//! ever takes the next ready stream.
//!
//! ## The certificate
//!
//! [`CertResolver`] holds the current certificate behind a lock and is what
//! rustls asks on every handshake, so the worker swaps a renewed certificate
//! in (or takes it away) without rebinding a single listener. With none
//! loaded a TLS client is simply closed: there is nothing honest to present.
//!
//! Unix only, like the worker: rustls is only in the dependency graph off
//! Windows (see Cargo.toml).

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

use crate::remote::debuglog;
use crate::web::server::PeerInfo;

/// The first byte of every TLS connection: a handshake record (RFC 8446
/// §5.1, `ContentType.handshake`). No HTTP method starts with it.
pub const TLS_HANDSHAKE_RECORD: u8 = 0x16;

/// How long a new connection may stay silent before it is handed to plain
/// HTTP anyway. Every real client speaks first and at once (a browser's
/// ClientHello, a Desktop's request line); a silent one is treated exactly as
/// it was before sniffing existed.
pub const SNIFF_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a TLS handshake may take before the connection is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How many sniffed connections may wait for axum to take them.
const READY_QUEUE: usize = 64;

// ===========================================================================
// The certificate
// ===========================================================================

/// The certificate rustls presents, swappable while the server runs.
#[derive(Default)]
pub struct CertResolver {
    current: RwLock<Option<Arc<CertifiedKey>>>,
    /// Bumped on every [`CertResolver::set`], so "log once per change" has
    /// something to compare against.
    generation: AtomicU64,
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertResolver")
            .field("loaded", &self.is_loaded())
            .field("generation", &self.generation())
            .finish()
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl CertResolver {
    /// Present `key` from the next handshake on (`None`: refuse TLS).
    pub fn set(&self, key: Option<Arc<CertifiedKey>>) {
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = key;
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether a certificate is loaded.
    pub fn is_loaded(&self) -> bool {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// How many times the certificate was set.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

/// A certificate chain and its private key, both PEM, as rustls serves them.
/// Refused when either is missing or unreadable, or when the key is not the
/// certificate's.
pub fn certified_key(cert_pem: &str, key_pem: &str) -> Result<CertifiedKey, String> {
    let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("the certificate is not valid PEM: {e}"))?;
    if chain.is_empty() {
        return Err("no certificate in the PEM".to_string());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|e| format!("the private key is not valid PEM: {e}"))?;
    CertifiedKey::from_der(chain, key, &rustls::crypto::aws_lc_rs::default_provider())
        .map_err(|e| format!("the certificate and key cannot be used: {e}"))
}

/// Everything a sniffing listener needs for its TLS half: one per server,
/// shared by every listener, so one [`CertResolver::set`] reaches them all.
pub struct Tls {
    resolver: Arc<CertResolver>,
    acceptor: TlsAcceptor,
    /// The resolver generation a "no certificate" refusal was last logged
    /// for, plus one (zero: never logged).
    refusal_logged: AtomicU64,
}

impl Tls {
    /// A TLS acceptor over a fresh, empty [`CertResolver`]. HTTP/1.1 only
    /// (ALPN), like the server itself: a browser must not try HTTP/2, which
    /// the server does not speak and over which `/ws` would not upgrade.
    pub fn new() -> Result<Tls, String> {
        let resolver = Arc::new(CertResolver::default());
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS is unavailable: {e}"))?
        .with_no_client_auth()
        .with_cert_resolver(Arc::clone(&resolver) as Arc<dyn ResolvesServerCert>);
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Tls {
            resolver,
            acceptor: TlsAcceptor::from(Arc::new(config)),
            refusal_logged: AtomicU64::new(0),
        })
    }

    /// The certificate this server presents.
    pub fn resolver(&self) -> &CertResolver {
        &self.resolver
    }

    /// Log a TLS client refused for want of a certificate, once per
    /// certificate change rather than once per connection: a browser that
    /// keeps retrying must not flood the log.
    fn note_refused(&self, peer: SocketAddr) {
        let marker = self.resolver.generation() + 1;
        if self.refusal_logged.swap(marker, Ordering::Relaxed) != marker {
            debuglog::log(&format!(
                "web TLS refused address={} reason=no-certificate (logged once until the \
                 certificate changes)",
                peer.ip()
            ));
        }
    }
}

// ===========================================================================
// The stream
// ===========================================================================

/// One accepted connection: plain TCP or TLS over it.
pub enum MaybeTls {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeTls {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTls::Tls(s) => Pin::new(&mut **s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTls {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTls::Tls(s) => Pin::new(&mut **s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTls::Tls(s) => Pin::new(&mut **s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTls::Tls(s) => Pin::new(&mut **s).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            MaybeTls::Tls(s) => Pin::new(&mut **s).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            MaybeTls::Plain(s) => s.is_write_vectored(),
            MaybeTls::Tls(s) => s.is_write_vectored(),
        }
    }
}

// ===========================================================================
// The listener
// ===========================================================================

/// Aborts the accept loop when the listener goes, which is what releases
/// the port: dropping [`SniffListener`] (axum does, on shutdown, and a
/// retired Tailscale listener's task is aborted) must close the socket.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What axum serves: the connections [`sniff`] has already classified.
pub struct SniffListener {
    ready: mpsc::Receiver<(MaybeTls, PeerInfo)>,
    local: SocketAddr,
    _accept: AbortOnDrop,
}

impl axum::serve::Listener for SniffListener {
    type Io = MaybeTls;
    type Addr = PeerInfo;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(ready) => ready,
            // The accept loop is gone (its listener failed for good): there
            // will be no further connection, so wait for the shutdown.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(PeerInfo {
            addr: self.local,
            local_port: self.local.port(),
            tls: false,
        })
    }
}

impl Connected<IncomingStream<'_, SniffListener>> for PeerInfo {
    fn connect_info(stream: IncomingStream<'_, SniffListener>) -> Self {
        *stream.remote_addr()
    }
}

/// Wrap a bound listener so every connection is sniffed: TLS ones are
/// handshaken with `tls` (or closed when it has no certificate), the rest are
/// served as plain HTTP. Must be called inside the runtime.
pub fn sniff(listener: TcpListener, tls: Arc<Tls>) -> io::Result<SniffListener> {
    let local = listener.local_addr()?;
    let (tx, ready) = mpsc::channel(READY_QUEUE);
    let accept = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    tokio::spawn(classify(stream, peer, local, Arc::clone(&tls), tx.clone()));
                }
                // Out of descriptors, a connection reset before accept: as
                // axum's own loop does, pause and carry on.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    });
    Ok(SniffListener {
        ready,
        local,
        _accept: AbortOnDrop(accept),
    })
}

/// Decide what one connection is and hand it on, or drop it.
async fn classify(
    stream: TcpStream,
    peer: SocketAddr,
    local: SocketAddr,
    tls: Arc<Tls>,
    ready: mpsc::Sender<(MaybeTls, PeerInfo)>,
) {
    let mut first = [0u8; 1];
    let is_tls = match tokio::time::timeout(SNIFF_TIMEOUT, stream.peek(&mut first)).await {
        // Closed before it said anything.
        Ok(Ok(0)) | Ok(Err(_)) => return,
        Ok(Ok(_)) => first[0] == TLS_HANDSHAKE_RECORD,
        // Silent: plain HTTP, exactly as before there was any sniffing.
        Err(_) => false,
    };
    let info = PeerInfo {
        addr: peer,
        local_port: local.port(),
        tls: is_tls,
    };
    let io = if is_tls {
        if !tls.resolver.is_loaded() {
            tls.note_refused(peer);
            return;
        }
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, tls.acceptor.accept(stream)).await {
            Ok(Ok(stream)) => MaybeTls::Tls(Box::new(stream)),
            Ok(Err(e)) => {
                debuglog::log(&format!(
                    "web TLS handshake failed address={} err={e}",
                    peer.ip()
                ));
                return;
            }
            Err(_) => return,
        }
    } else {
        MaybeTls::Plain(stream)
    };
    // A closed channel means the server is shutting down: drop it.
    let _ = ready.send((io, info)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &str = include_str!("../../tests/fixtures/tls/cert.pem");
    const KEY: &str = include_str!("../../tests/fixtures/tls/key.pem");

    #[test]
    fn the_fixture_loads_and_a_mismatched_or_broken_pair_is_refused() {
        assert!(certified_key(CERT, KEY).is_ok());
        assert!(certified_key("", KEY).is_err());
        assert!(certified_key(CERT, "").is_err());
        assert!(certified_key(KEY, CERT).is_err(), "swapped");
        assert!(certified_key(
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
            KEY
        )
        .is_err());
    }

    #[test]
    fn the_resolver_swaps_without_a_rebind() {
        let tls = Tls::new().expect("aws-lc-rs is available");
        let resolver = tls.resolver();
        assert!(!resolver.is_loaded());
        let generation = resolver.generation();
        resolver.set(Some(Arc::new(certified_key(CERT, KEY).unwrap())));
        assert!(resolver.is_loaded());
        assert_eq!(resolver.generation(), generation + 1);
        resolver.set(None);
        assert!(!resolver.is_loaded());
        assert_eq!(resolver.generation(), generation + 2);
    }
}
