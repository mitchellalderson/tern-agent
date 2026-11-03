//! TLS for the TCP listener (deployment spec §6.1): a self-signed
//! certificate that clients pin (`tls = "auto"`), or certificate files.
//! Also the client side of pinning, used by `tern --remote` and
//! `tern devices join`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tern_core::TernHome;
use tern_core::config::{ApiConfig, TlsMode};

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// `sha256:` and the upper-case hex SHA-256 of a DER certificate, as put in
/// pairing links.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, der);
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02X}")).collect();
    format!("sha256:{hex}")
}

/// Parse a pin as written by [`fingerprint`]. Hex may be lower case and
/// separated by colons.
pub fn parse_pin(pin: &str) -> anyhow::Result<[u8; 32]> {
    let hex: String = pin
        .strip_prefix("sha256:")
        .unwrap_or(pin)
        .chars()
        .filter(|c| *c != ':')
        .collect();
    let mut out = [0u8; 32];
    if hex.len() != 64 || !hex.is_ascii() {
        bail!("certificate pin must be sha256: and 64 hex digits");
    }
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .context("certificate pin must be sha256: and 64 hex digits")?;
    }
    Ok(out)
}

/// This machine's name, for certificates and pairing links.
pub fn host_name() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

fn tls_dir(home: &TernHome) -> PathBuf {
    home.root().join("tls")
}

/// The `auto` certificate and key, created on first use. Returns the
/// certificate path and key path.
pub fn ensure_auto_cert(home: &TernHome) -> anyhow::Result<(PathBuf, PathBuf)> {
    let dir = tls_dir(home);
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    if cert.exists() && key.exists() {
        return Ok((cert, key));
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut names = vec!["localhost".to_owned(), "127.0.0.1".into(), "::1".into()];
    let host = host_name();
    if !names.contains(&host) {
        names.push(host);
    }
    let made = rcgen::generate_simple_self_signed(names).context("generating a certificate")?;
    write_private(&key, made.signing_key.serialize_pem().as_bytes())?;
    write_private(&cert, made.cert.pem().as_bytes())?;
    tracing::info!(cert = %cert.display(), "generated a self-signed TLS certificate");
    Ok((cert, key))
}

/// Write via a temporary file and rename, mode 0600.
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn load_certs(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let certs = CertificateDer::pem_slice_iter(&pem)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parsing {}", path.display()))?;
    if certs.is_empty() {
        bail!("no certificates in {}", path.display());
    }
    Ok(certs)
}

/// The pin clients should use for this core: the `auto` certificate's
/// fingerprint, or `None` for `files` and `off` (verified normally).
pub fn pin(home: &TernHome, api: &ApiConfig) -> anyhow::Result<Option<String>> {
    if api.tls != TlsMode::Auto {
        return Ok(None);
    }
    let (cert, _) = ensure_auto_cert(home)?;
    let certs = load_certs(&cert)?;
    Ok(certs.first().map(|c| fingerprint(c.as_ref())))
}

/// The server's TLS configuration, or `None` for `tls = "off"`.
pub fn server_config(
    home: &TernHome,
    api: &ApiConfig,
) -> anyhow::Result<Option<Arc<rustls::ServerConfig>>> {
    let (cert, key) = match api.tls {
        TlsMode::Off => return Ok(None),
        TlsMode::Auto => ensure_auto_cert(home)?,
        TlsMode::Files => {
            let (Some(c), Some(k)) = (&api.tls_cert, &api.tls_key) else {
                bail!("api.tls = \"files\" needs api.tls_cert and api.tls_key");
            };
            (
                tern_core::home::expand_tilde(Path::new(c)),
                tern_core::home::expand_tilde(Path::new(k)),
            )
        }
    };
    let certs = load_certs(&cert)?;
    let key = PrivateKeyDer::from_pem_file(&key)
        .with_context(|| format!("reading a private key from {}", key.display()))?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("loading the TLS certificate")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Some(Arc::new(config)))
}

/// Accepts exactly one certificate: the one whose SHA-256 is the pin.
/// Names and CAs don't matter; signatures are still checked.
#[derive(Debug)]
pub struct PinnedVerifier {
    pin: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl PinnedVerifier {
    pub fn new(pin: [u8; 32]) -> Self {
        Self {
            pin,
            algorithms: provider().signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, end_entity.as_ref());
        if digest.as_ref() == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the core's certificate doesn't match the pinned fingerprint".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// An HTTP client for a remote core: pinned when `pin` is given, else
/// verified against the usual roots.
pub fn client(pin: Option<&str>) -> anyhow::Result<reqwest::ClientBuilder> {
    let builder = tern_core::net::http_client();
    let Some(pin) = pin else {
        return Ok(builder);
    };
    let pin = parse_pin(pin)?;
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier::new(pin)))
        .with_no_client_auth();
    Ok(builder.tls_backend_preconfigured(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_round_trip_and_reject_junk() {
        let fp = fingerprint(b"hello");
        assert!(fp.starts_with("sha256:"));
        assert_eq!(fp.len(), 7 + 64);
        let raw = parse_pin(&fp).unwrap();
        assert_eq!(parse_pin(&fp.to_lowercase()).unwrap(), raw);
        let colons: Vec<String> = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_pin(&colons.join(":")).unwrap(), raw);
        assert!(parse_pin("sha256:abcd").is_err());
        assert!(parse_pin(&format!("sha256:{}", "zz".repeat(32))).is_err());
    }

    #[test]
    fn auto_cert_is_created_once_and_private() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path().join(".tern"));
        let api = ApiConfig::default();
        let first = pin(&home, &api).unwrap().unwrap();
        assert_eq!(pin(&home, &api).unwrap().unwrap(), first, "stable");
        assert!(server_config(&home, &api).unwrap().is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.root().join("tls/key.pem"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
