//! Optional TCP+TLS transport for the intent bus.
//!
//! Enabled via the `tls` feature flag. Provides `TlsBusServer` and `TlsBusClient`
//! for encrypted remote connections, complementing the default Unix socket transport.

use crate::message::BusMessage;
use crate::transport::{read_message, write_message};
use agentos_types::AgentOSError;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// A TLS-encrypted TCP bus server for remote agent connections.
pub struct TlsBusServer {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    bind_addr: SocketAddr,
}

impl TlsBusServer {
    /// Bind a TLS server on the given address using the provided cert and key files.
    pub async fn bind(
        addr: &str,
        cert_path: &Path,
        key_path: &Path,
    ) -> Result<Self, AgentOSError> {
        let certs = load_certs(cert_path)?;
        let key = load_private_key(key_path)?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| AgentOSError::BusError(format!("TLS config error: {}", e)))?;

        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| AgentOSError::BusError(format!("Failed to bind TCP: {}", e)))?;

        let bind_addr = listener
            .local_addr()
            .map_err(|e| AgentOSError::BusError(format!("Failed to get local addr: {}", e)))?;

        tracing::info!("TLS Intent Bus listening on {}", bind_addr);

        Ok(Self {
            listener,
            acceptor,
            bind_addr,
        })
    }

    /// Returns the bound address (useful when binding to port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    /// Accept a single TLS connection.
    pub async fn accept(&self) -> Result<TlsBusConnection, AgentOSError> {
        let (tcp_stream, peer_addr) = self
            .listener
            .accept()
            .await
            .map_err(|e| AgentOSError::BusError(format!("TCP accept failed: {}", e)))?;

        let tls_stream = self
            .acceptor
            .accept(tcp_stream)
            .await
            .map_err(|e| AgentOSError::BusError(format!("TLS handshake failed: {}", e)))?;

        tracing::debug!("TLS connection accepted from {}", peer_addr);
        Ok(TlsBusConnection {
            stream: tls_stream,
        })
    }
}

/// A single TLS-encrypted bus connection.
pub struct TlsBusConnection {
    stream: tokio_rustls::server::TlsStream<TcpStream>,
}

impl TlsBusConnection {
    pub async fn read(&mut self) -> Result<BusMessage, AgentOSError> {
        read_message(&mut self.stream).await
    }

    pub async fn write(&mut self, msg: &BusMessage) -> Result<(), AgentOSError> {
        write_message(&mut self.stream, msg).await
    }
}

/// A TLS-encrypted TCP bus client for remote connections.
pub struct TlsBusClient {
    stream: tokio_rustls::client::TlsStream<TcpStream>,
}

impl TlsBusClient {
    /// Connect to a TLS bus server using a custom CA certificate for verification.
    pub async fn connect(
        addr: &str,
        server_name: &str,
        ca_cert_path: &Path,
    ) -> Result<Self, AgentOSError> {
        let mut root_store = tokio_rustls::rustls::RootCertStore::empty();
        let ca_certs = load_certs(ca_cert_path)?;
        for cert in ca_certs {
            root_store
                .add(cert)
                .map_err(|e| AgentOSError::BusError(format!("Failed to add CA cert: {}", e)))?;
        }

        let config = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

        let tcp_stream = TcpStream::connect(addr)
            .await
            .map_err(|e| AgentOSError::BusError(format!("TCP connect failed: {}", e)))?;

        let domain = server_name
            .to_string()
            .try_into()
            .map_err(|e| AgentOSError::BusError(format!("Invalid server name: {}", e)))?;

        let tls_stream = connector
            .connect(domain, tcp_stream)
            .await
            .map_err(|e| AgentOSError::BusError(format!("TLS connect failed: {}", e)))?;

        Ok(Self {
            stream: tls_stream,
        })
    }

    pub async fn send_message(&mut self, msg: &BusMessage) -> Result<(), AgentOSError> {
        write_message(&mut self.stream, msg).await
    }

    pub async fn receive_message(&mut self) -> Result<BusMessage, AgentOSError> {
        read_message(&mut self.stream).await
    }
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, AgentOSError> {
    let file = std::fs::File::open(path)
        .map_err(|e| AgentOSError::BusError(format!("Cannot open cert file {:?}: {}", path, e)))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentOSError::BusError(format!("Failed to parse certs: {}", e)))
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, AgentOSError> {
    let file = std::fs::File::open(path)
        .map_err(|e| AgentOSError::BusError(format!("Cannot open key file {:?}: {}", path, e)))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| AgentOSError::BusError(format!("Failed to parse private key: {}", e)))?
        .ok_or_else(|| AgentOSError::BusError("No private key found in file".to_string()))
}
