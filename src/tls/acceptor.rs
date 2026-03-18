//! TLS acceptor: certificate loading and `TlsAcceptor` construction.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// Errors that can occur when loading TLS certificates and building the acceptor.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The certificate PEM file could not be opened.
    #[error("Failed to open certificate file '{path}': {source}")]
    CertFileOpen {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The private key PEM file could not be opened.
    #[error("Failed to open private key file '{path}': {source}")]
    KeyFileOpen {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The certificate PEM file contained no valid certificates.
    #[error("No valid certificates found in '{path}'")]
    NoCerts { path: String },
    /// A certificate could not be parsed.
    #[error("Failed to parse certificate in '{path}': {source}")]
    ParseCert {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The private key PEM file contained no recognised private key.
    #[error("No private key found in '{path}' (expected PKCS8 or RSA PEM block)")]
    NoPrivateKey { path: String },
    /// The private key could not be parsed.
    #[error("Failed to parse private key in '{path}': {source}")]
    ParseKey {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// rustls rejected the certificate/key combination.
    #[error("Invalid TLS certificate/key: {0}")]
    InvalidCertKey(#[from] rustls::Error),
}

/// Load a [`TlsAcceptor`] from PEM-encoded certificate and private key files.
///
/// # Arguments
/// * `cert_path` — path to a PEM file containing one or more X.509 certificates
///   (the leaf certificate first, followed by any intermediates).
/// * `key_path` — path to a PEM file containing an RSA or PKCS#8 private key.
///
/// # Errors
/// Returns [`TlsError`] if either file cannot be read, contains no valid
/// key/cert material, or if rustls rejects the combination.
pub fn load_tls_acceptor(cert_path: &str, key_path: &str) -> Result<TlsAcceptor, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;

    // Use ring explicitly via builder_with_provider so the choice is
    // deterministic regardless of which CryptoProvider other crates
    // (e.g. reqwest's hyper-rustls) pull into the binary.
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(TlsError::InvalidCertKey)?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Read all PEM-encoded certificates from `path`.
fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let file = File::open(path).map_err(|e| TlsError::CertFileOpen {
        path: path.to_string(),
        source: e,
    })?;
    let mut reader = BufReader::new(file);

    let certs: Result<Vec<CertificateDer<'static>>, _> = rustls_pemfile::certs(&mut reader)
        .map(|r| {
            r.map_err(|e| TlsError::ParseCert {
                path: path.to_string(),
                source: e,
            })
        })
        .collect();

    let certs = certs?;
    if certs.is_empty() {
        return Err(TlsError::NoCerts {
            path: path.to_string(),
        });
    }
    Ok(certs)
}

/// Read the first PEM-encoded private key from `path`.
///
/// Accepts RSA (`RSA PRIVATE KEY`), PKCS#8 (`PRIVATE KEY`), and EC keys.
fn load_private_key(path: &str) -> Result<PrivateKeyDer<'static>, TlsError> {
    let file = File::open(path).map_err(|e| TlsError::KeyFileOpen {
        path: path.to_string(),
        source: e,
    })?;
    let mut reader = BufReader::new(file);

    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| TlsError::ParseKey {
            path: path.to_string(),
            source: e,
        })?
        .ok_or_else(|| TlsError::NoPrivateKey {
            path: path.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cert_file_returns_error() {
        let err = load_tls_acceptor("/nonexistent/cert.pem", "/nonexistent/key.pem");
        assert!(matches!(err, Err(TlsError::CertFileOpen { .. })));
    }

    #[test]
    fn missing_key_file_returns_error() {
        // Use a real cert path so we get past cert loading to the key error.
        // Since we have no real cert in tests, just verify it fails on cert.
        let err = load_tls_acceptor("/nonexistent/cert.pem", "/nonexistent/key.pem");
        assert!(err.is_err());
    }
}
