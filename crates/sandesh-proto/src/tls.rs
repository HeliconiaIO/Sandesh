//! TLS for the protocol servers (Phase 4.5, ADR-034).
//!
//! `rustls` rather than OpenSSL: no C toolchain to install, no `unsafe` in the
//! handshake, and it is what the async ecosystem already assumes.
//!
//! ## Certificates a developer does not have to think about
//!
//! A local mail server that demanded a certificate before it would start would
//! be unusable — nobody has a CA-signed certificate for `localhost`. So Sandesh
//! generates a self-signed one on first use and persists it to the data
//! directory, alongside the database (ADR-031). It is reused on every
//! subsequent start, because a certificate that changed on every restart would
//! retrain the operator to click through warnings, and would invalidate any
//! trust they had deliberately granted.
//!
//! A supplied certificate and key are used when configured, which is the path
//! for anyone who has a real one.
//!
//! ## What is deliberately not here
//!
//! No client-certificate verification, and no decision about *when* TLS is
//! required. This module can hand out an acceptor; whether a given connection
//! must use one is the session's policy (ADR-033).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

/// The certificate file created inside the data directory.
pub const CERTIFICATE_FILE: &str = "sandesh-cert.pem";

/// The private key file created inside the data directory.
pub const KEY_FILE: &str = "sandesh-key.pem";

/// The names a generated certificate is valid for.
///
/// A developer reaches a local server by several names depending on the client,
/// and a certificate that covers only one of them produces a mismatch warning
/// for the others.
const GENERATED_SUBJECTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// Why TLS could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A certificate or key file could not be read or written.
    #[error("cannot access {path}: {source}")]
    Io {
        /// The file involved.
        path: String,
        /// The underlying cause.
        source: io::Error,
    },

    /// A supplied certificate or key could not be parsed.
    #[error("{path} is not a valid PEM {kind}")]
    Pem {
        /// The file involved.
        path: String,
        /// `certificate` or `private key`.
        kind: &'static str,
    },

    /// Generating a self-signed certificate failed.
    #[error("cannot generate a self-signed certificate: {0}")]
    Generation(String),

    /// `rustls` rejected the certificate and key pair.
    #[error("the certificate and key are not a usable pair: {0}")]
    Configuration(String),
}

/// A certificate chain and its private key.
pub struct Identity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl std::fmt::Debug for Identity {
    /// The private key never renders, for the same reason a password does not.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("certificates", &self.chain.len())
            .field("key", &"[redacted]")
            .finish()
    }
}

impl Identity {
    /// Loads the identity from the data directory, generating one if absent.
    ///
    /// The generated certificate is written with owner-only permissions on
    /// Unix: a private key readable by every user on the machine is not a
    /// private key.
    ///
    /// # Errors
    /// [`TlsError`] when the files cannot be read, written, parsed or generated.
    pub fn load_or_generate(data_dir: &Path) -> Result<Self, TlsError> {
        let cert_path = data_dir.join(CERTIFICATE_FILE);
        let key_path = data_dir.join(KEY_FILE);

        if cert_path.exists() && key_path.exists() {
            return Self::from_files(&cert_path, &key_path);
        }

        let generated = generate_self_signed()?;

        std::fs::create_dir_all(data_dir).map_err(|source| TlsError::Io {
            path: data_dir.display().to_string(),
            source,
        })?;

        write_file(&cert_path, generated.certificate_pem.as_bytes(), false)?;
        // The key is written owner-only, before anything else can read it.
        write_file(&key_path, generated.key_pem.as_bytes(), true)?;

        Self::from_files(&cert_path, &key_path)
    }

    /// Loads a supplied certificate and key.
    ///
    /// # Errors
    /// [`TlsError`] when either file cannot be read or parsed.
    pub fn from_files(cert_path: &Path, key_path: &Path) -> Result<Self, TlsError> {
        let cert_pem = std::fs::read(cert_path).map_err(|source| TlsError::Io {
            path: cert_path.display().to_string(),
            source,
        })?;
        let key_pem = std::fs::read(key_path).map_err(|source| TlsError::Io {
            path: key_path.display().to_string(),
            source,
        })?;

        let chain = CertificateDer::pem_slice_iter(&cert_pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TlsError::Pem {
                path: cert_path.display().to_string(),
                kind: "certificate",
            })?;

        if chain.is_empty() {
            return Err(TlsError::Pem {
                path: cert_path.display().to_string(),
                kind: "certificate",
            });
        }

        let key = PrivateKeyDer::from_pem_slice(&key_pem).map_err(|_| TlsError::Pem {
            path: key_path.display().to_string(),
            kind: "private key",
        })?;

        Ok(Self { chain, key })
    }

    /// Builds an acceptor from this identity.
    ///
    /// # Errors
    /// [`TlsError::Configuration`] when the certificate and key do not match.
    pub fn acceptor(self) -> Result<TlsAcceptor, TlsError> {
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(self.chain, self.key)
            .map_err(|error| TlsError::Configuration(error.to_string()))?;

        Ok(TlsAcceptor::from(Arc::new(config)))
    }

    /// The certificate's SHA-256 fingerprint, uppercase hex, colon-separated.
    ///
    /// Displayed at startup so an operator can compare it against what their
    /// client reports, which is the only way to trust a self-signed
    /// certificate deliberately rather than by clicking through a warning.
    #[must_use]
    pub fn fingerprint(&self) -> Option<String> {
        use sha2::{Digest, Sha256};

        let leaf = self.chain.first()?;
        let digest = Sha256::digest(leaf.as_ref());

        Some(
            digest
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(":"),
        )
    }
}

/// A freshly generated self-signed certificate.
struct Generated {
    certificate_pem: String,
    key_pem: String,
}

/// Generates a self-signed certificate for the local names.
fn generate_self_signed() -> Result<Generated, TlsError> {
    let subjects = GENERATED_SUBJECTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();

    let certified = rcgen::generate_simple_self_signed(subjects)
        .map_err(|error| TlsError::Generation(error.to_string()))?;

    Ok(Generated {
        certificate_pem: certified.cert.pem(),
        key_pem: certified.signing_key.serialize_pem(),
    })
}

/// Writes a file, restricting it to its owner when it holds a private key.
fn write_file(path: &Path, contents: &[u8], private: bool) -> Result<(), TlsError> {
    std::fs::write(path, contents).map_err(|source| TlsError::Io {
        path: path.display().to_string(),
        source,
    })?;

    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;

        // 0600. A world-readable private key is equivalent to no key at all,
        // and the default umask does not guarantee this.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| TlsError::Io {
                path: path.display().to_string(),
                source,
            },
        )?;
    }

    #[cfg(not(unix))]
    let _ = private;

    Ok(())
}

/// Where the certificate and key live for a given data directory.
#[must_use]
pub fn paths(data_dir: &Path) -> (PathBuf, PathBuf) {
    (data_dir.join(CERTIFICATE_FILE), data_dir.join(KEY_FILE))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_certificate_is_generated_on_first_use() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let identity = Identity::load_or_generate(dir.path()).expect("generates");

        let (cert, key) = paths(dir.path());
        assert!(cert.exists(), "the certificate was written");
        assert!(key.exists(), "the key was written");
        assert!(identity.fingerprint().is_some());
    }

    #[test]
    fn the_certificate_persists_across_restarts() {
        // A certificate that changed every start would retrain the operator to
        // click through warnings, and would break any trust they had granted.
        let dir = tempfile::tempdir().expect("temporary directory");

        let first = Identity::load_or_generate(dir.path()).expect("generates");
        let second = Identity::load_or_generate(dir.path()).expect("reuses");

        assert_eq!(
            first.fingerprint(),
            second.fingerprint(),
            "the second start must reuse the stored certificate"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_private_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temporary directory");
        Identity::load_or_generate(dir.path()).expect("generates");

        let (_, key) = paths(dir.path());
        let mode = std::fs::metadata(&key)
            .expect("key metadata")
            .permissions()
            .mode();

        assert_eq!(
            mode & 0o077,
            0,
            "the key must not be group- or world-readable"
        );
    }

    #[test]
    fn a_generated_identity_builds_an_acceptor() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let identity = Identity::load_or_generate(dir.path()).expect("generates");
        assert!(identity.acceptor().is_ok());
    }

    #[test]
    fn the_private_key_never_appears_in_debug_output() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let identity = Identity::load_or_generate(dir.path()).expect("generates");

        let rendered = format!("{identity:?}");
        assert!(rendered.contains("[redacted]"), "got {rendered}");
        assert!(!rendered.contains("PRIVATE KEY"));
    }

    #[test]
    fn a_malformed_certificate_is_rejected() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let (cert, key) = paths(dir.path());
        std::fs::write(&cert, b"not a certificate").expect("writes");
        std::fs::write(&key, b"not a key").expect("writes");

        assert!(Identity::from_files(&cert, &key).is_err());
    }

    #[test]
    fn the_fingerprint_is_colon_separated_hex() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let identity = Identity::load_or_generate(dir.path()).expect("generates");
        let fingerprint = identity.fingerprint().expect("has one");

        // SHA-256 is 32 bytes, so 32 hex pairs joined by 31 colons.
        assert_eq!(fingerprint.split(':').count(), 32, "got {fingerprint}");
        assert!(
            fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':')
        );
    }
}
