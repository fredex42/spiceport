//! TLS for SPICE channels with rustls, verifying the server the way
//! spice-common's `ssl_verify.c` does:
//!
//! - the certificate chain must lead to a trusted CA (the `--ca-file`
//!   bundle, or the system roots). As with OpenSSL's
//!   `X509_V_FLAG_PARTIAL_CHAIN`, a certificate listed verbatim in the CA
//!   file is trusted even if it is self-signed or not a CA;
//! - then either the subject must equal `--host-subject` exactly
//!   (`verify_subject`), or the host name must match a subjectAltName DNS
//!   or IP entry, falling back to the common name only when there are no
//!   such entries (`verify_hostname`, RFC 2818 with wildcards).

use std::sync::Arc;

use log::{debug, info, warn};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore,
    SignatureScheme,
};
use tokio::net::TcpStream;
use x509_parser::prelude::*;

use crate::error::{ClientError, Result};
use crate::session::{Shared, TlsOptions};
use crate::transport::Transport;

pub async fn connect(shared: &Shared, host: &str, tcp: TcpStream) -> Result<Transport> {
    let config = shared.tls_config()?;
    let name = ServerName::try_from(host.to_string())
        .map_err(|e| ClientError::Tls(format!("bad host name '{host}': {e}")))?;
    let connector = tokio_rustls::TlsConnector::from(config);
    let stream = connector
        .connect(name, tcp)
        .await
        .map_err(|e| ClientError::Tls(format!("handshake with {host} failed: {e}")))?;
    let (_, conn) = stream.get_ref();
    debug!(
        "TLS established: {:?}, {:?}",
        conn.protocol_version(),
        conn.negotiated_cipher_suite().map(|c| c.suite())
    );
    Ok(Transport::Tls(Box::new(stream)))
}

/// Build the rustls client configuration for a session.
pub fn client_config(opts: &TlsOptions) -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    let mut pinned = Vec::new();
    match &opts.ca_file {
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| ClientError::Tls(format!("reading {}: {e}", path.display())))?;
            for cert in rustls_pemfile::certs(&mut &pem[..]) {
                let cert =
                    cert.map_err(|e| ClientError::Tls(format!("parsing {}: {e}", path.display())))?;
                pinned.push(cert.clone());
                // Not every certificate is usable as an anchor (e.g. a
                // self-signed leaf); those are still trusted via `pinned`.
                if let Err(e) = roots.add(cert) {
                    debug!("CA file entry not usable as a trust anchor: {e}");
                }
            }
            if pinned.is_empty() {
                return Err(ClientError::Tls(format!(
                    "no certificates in {}",
                    path.display()
                )));
            }
            info!(
                "using {} certificate(s) from {}",
                pinned.len(),
                path.display()
            );
        }
        None => {
            let native = rustls_native_certs::load_native_certs();
            for e in &native.errors {
                warn!("loading system certificates: {e}");
            }
            let (ok, bad) = roots.add_parsable_certificates(native.certs);
            debug!("loaded {ok} system root certificates ({bad} unusable)");
        }
    }
    let webpki: Arc<dyn ServerCertVerifier> = if roots.is_empty() {
        Arc::new(NoRoots)
    } else {
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|e| ClientError::Tls(e.to_string()))?
    };
    let verifier = Arc::new(SpiceVerifier {
        webpki,
        pinned,
        subject: opts.host_subject.clone(),
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| ClientError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Stands in for webpki when there are no roots at all.
#[derive(Debug)]
struct NoRoots;

impl ServerCertVerifier for NoRoots {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        Err(TlsError::InvalidCertificate(
            CertificateError::UnknownIssuer,
        ))
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("no roots".into()))
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("no roots".into()))
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        Vec::new()
    }
}

#[derive(Debug)]
struct SpiceVerifier {
    webpki: Arc<dyn ServerCertVerifier>,
    /// Certificates from the CA file, trusted verbatim (partial chain).
    pinned: Vec<CertificateDer<'static>>,
    subject: Option<String>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

fn is_name_error(e: &TlsError) -> bool {
    matches!(
        e,
        TlsError::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
        )
    )
}

impl ServerCertVerifier for SpiceVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        let pinned = self
            .pinned
            .iter()
            .any(|c| c.as_ref() == end_entity.as_ref());
        // Chain check; a name mismatch is decided below with spice's rules.
        let chain_ok =
            match self
                .webpki
                .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
            {
                Ok(_) => true,
                Err(e) if is_name_error(&e) => true,
                Err(e) if pinned => {
                    debug!("certificate is pinned by the CA file ({e})");
                    true
                }
                Err(e) => {
                    warn!("ssl: server certificate verification failed: {e}");
                    return Err(e);
                }
            };
        debug_assert!(chain_ok);
        let (_, cert) = X509Certificate::from_der(end_entity.as_ref())
            .map_err(|_| TlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        match &self.subject {
            Some(subject) => {
                if verify_subject(&cert, subject) {
                    Ok(ServerCertVerified::assertion())
                } else {
                    warn!(
                        "ssl: subject '{subject}' verification failed (certificate: {})",
                        cert.subject()
                    );
                    Err(TlsError::InvalidCertificate(
                        CertificateError::NotValidForName,
                    ))
                }
            }
            None => {
                let host = match server_name {
                    ServerName::DnsName(d) => d.as_ref().to_string(),
                    ServerName::IpAddress(ip) => std::net::IpAddr::from(*ip).to_string(),
                    _ => String::new(),
                };
                if verify_hostname(&cert, &host) {
                    Ok(ServerCertVerified::assertion())
                } else {
                    warn!("ssl: hostname '{host}' verification failed");
                    Err(TlsError::InvalidCertificate(
                        CertificateError::NotValidForName,
                    ))
                }
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// `_gnutls_hostname_compare`: case-insensitive, `*` matches within one
/// domain component.
fn hostname_matches(pattern: &str, host: &str) -> bool {
    let (p, h) = (pattern.as_bytes(), host.as_bytes());
    let mut i = 0;
    while i < p.len() && i < h.len() && p[i] != b'*' && p[i].eq_ignore_ascii_case(&h[i]) {
        i += 1;
    }
    if i == p.len() && i == h.len() {
        return true;
    }
    if i < p.len() && p[i] == b'*' {
        let rest = &pattern[i + 1..];
        let mut hi = i;
        loop {
            if hostname_matches(rest, &host[hi..]) {
                return true;
            }
            if hi >= h.len() || h[hi] == b'.' {
                return false;
            }
            hi += 1;
        }
    }
    false
}

/// `verify_hostname`
fn verify_hostname(cert: &X509Certificate<'_>, host: &str) -> bool {
    let ip: Option<std::net::IpAddr> = host.parse().ok();
    let mut found_names = false;
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            match name {
                GeneralName::DNSName(dns) => {
                    found_names = true;
                    if hostname_matches(dns, host) {
                        debug!("alt name match={dns}");
                        return true;
                    }
                }
                GeneralName::IPAddress(bytes) => {
                    found_names = true;
                    let matches = match ip {
                        Some(std::net::IpAddr::V4(v4)) => *bytes == v4.octets(),
                        Some(std::net::IpAddr::V6(v6)) => *bytes == v6.octets(),
                        None => false,
                    };
                    if matches {
                        debug!("alt name IP match={host}");
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    if found_names {
        debug!("SubjectAltName mismatch");
        return false;
    }
    let cn_match = cert
        .subject()
        .iter_common_name()
        .filter_map(|cn| cn.as_str().ok())
        .any(|cn| hostname_matches(cn, host));
    if !cn_match {
        debug!("common name mismatch");
    }
    cn_match
}

/// `subject_to_x509_name`: parse `C=GB,O=Example,CN=host` (with `\,` and
/// `\\` escapes) into (key, value) pairs.
fn parse_subject(subject: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut key = String::new();
    let mut val = String::new();
    let mut in_value = false;
    let mut chars = subject.chars().peekable();
    loop {
        let mut c = chars.next();
        let mut escape = false;
        if c == Some('\\') {
            c = chars.next();
            if c != Some('\\') && c != Some(',') {
                return None;
            }
            escape = true;
        }
        if !in_value {
            match c {
                Some(' ') if key.is_empty() => continue,
                None if key.is_empty() => return Some(out),
                None => return None,
                Some(',') if !escape => return None,
                Some('=') if !escape => in_value = true,
                Some(ch) => key.push(ch),
            }
        } else {
            match c {
                None | Some(',') if !escape || c.is_none() => {
                    if val.is_empty() {
                        return None;
                    }
                    out.push((std::mem::take(&mut key), std::mem::take(&mut val)));
                    if c.is_none() {
                        return Some(out);
                    }
                    in_value = false;
                }
                Some(ch) => val.push(ch),
                None => unreachable!(),
            }
        }
    }
}

/// OpenSSL short and long names for the common attribute types.
fn attr_oid(key: &str) -> Option<&'static str> {
    Some(match key {
        "C" | "countryName" => "2.5.4.6",
        "ST" | "stateOrProvinceName" => "2.5.4.8",
        "L" | "localityName" => "2.5.4.7",
        "O" | "organizationName" => "2.5.4.10",
        "OU" | "organizationalUnitName" => "2.5.4.11",
        "CN" | "commonName" => "2.5.4.3",
        "emailAddress" => "1.2.840.113549.1.9.1",
        "serialNumber" => "2.5.4.5",
        "DC" | "domainComponent" => "0.9.2342.19200300.100.1.25",
        "UID" | "userId" => "0.9.2342.19200300.100.1.1",
        "street" | "streetAddress" => "2.5.4.9",
        "title" => "2.5.4.12",
        "GN" | "givenName" => "2.5.4.42",
        "SN" | "surname" => "2.5.4.4",
        _ => return None,
    })
}

/// `X509_NAME_cmp` on canonical encodings: same entries in the same order,
/// strings compared case-insensitively with whitespace runs collapsed.
fn canon(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `verify_subject`
fn verify_subject(cert: &X509Certificate<'_>, subject: &str) -> bool {
    let Some(wanted) = parse_subject(subject) else {
        warn!("ssl: cannot parse host subject '{subject}'");
        return false;
    };
    let actual: Vec<(String, String)> = cert
        .subject()
        .iter_attributes()
        .map(|a| {
            (
                a.attr_type().to_id_string(),
                a.as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    if actual.len() != wanted.len() {
        debug!(
            "subject mismatch: #entries cert={}, input={}",
            actual.len(),
            wanted.len()
        );
        return false;
    }
    actual.iter().zip(&wanted).all(|((oid, v), (k, w))| {
        let key_ok = attr_oid(k).map_or(oid == k, |o| o == oid);
        key_ok && canon(v) == canon(w)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(hostname_matches("example.com", "EXAMPLE.com"));
        assert!(hostname_matches("*.example.com", "a.example.com"));
        assert!(!hostname_matches("*.example.com", "a.b.example.com"));
        assert!(hostname_matches("a*.example.com", "abc.example.com"));
        assert!(!hostname_matches("example.com", "example.org"));
        assert!(!hostname_matches("*.example.com", "example.com"));
    }

    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
    use rustls::pki_types::PrivateKeyDer;

    struct Pki {
        dir: PathBuf,
        ca_pem: String,
        leaf_der: CertificateDer<'static>,
        leaf_key: Vec<u8>,
    }

    use std::path::PathBuf;

    /// A CA and a server certificate for `localhost` / 127.0.0.1 with the
    /// subject `CN=localhost` (rcgen doesn't keep attribute order, and the
    /// subject check is order-sensitive like OpenSSL's).
    fn pki(name: &str) -> Pki {
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.distinguished_name.push(DnType::CommonName, "test CA");
        let ca = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
        let mut leaf =
            CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
        leaf.distinguished_name
            .push(DnType::CommonName, "localhost");
        let key = KeyPair::generate().unwrap();
        let cert = leaf.signed_by(&key, &ca).unwrap();
        let dir = std::env::temp_dir().join(format!("spiceport-tls-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Pki {
            dir,
            ca_pem: ca.pem(),
            leaf_der: cert.der().clone(),
            leaf_key: key.serialize_der(),
        }
    }

    /// Serve one TLS handshake on a local port; returns the port.
    async fn serve(p: &Pki) -> u16 {
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![p.leaf_der.clone()],
            PrivateKeyDer::Pkcs8(p.leaf_key.clone().into()),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let _ = acceptor.accept(s).await;
            }
        });
        port
    }

    async fn try_connect(
        port: u16,
        host: &str,
        opts: &TlsOptions,
    ) -> std::result::Result<(), String> {
        let cfg = client_config(opts).map_err(|e| e.to_string())?;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let name = ServerName::try_from(host.to_string()).unwrap();
        tokio_rustls::TlsConnector::from(cfg)
            .connect(name, tcp)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn verification_rules() {
        let p = pki("rules");
        let ca_file = p.dir.join("ca.pem");
        std::fs::write(&ca_file, &p.ca_pem).unwrap();
        let port = serve(&p).await;
        let opts = |subject: Option<&str>| TlsOptions {
            ca_file: Some(ca_file.clone()),
            host_subject: subject.map(Into::into),
        };
        // SAN DNS and IP entries.
        try_connect(port, "localhost", &opts(None)).await.unwrap();
        try_connect(port, "127.0.0.1", &opts(None)).await.unwrap();
        // Wrong host name.
        assert!(try_connect(port, "example.com", &opts(None)).await.is_err());
        // --host-subject replaces the host name check.
        try_connect(port, "example.com", &opts(Some("CN=localhost")))
            .await
            .unwrap();
        assert!(
            try_connect(port, "localhost", &opts(Some("CN=other")))
                .await
                .is_err()
        );
        // An untrusted CA fails.
        let other = pki("other");
        let other_ca = other.dir.join("ca.pem");
        std::fs::write(&other_ca, &other.ca_pem).unwrap();
        let o = TlsOptions {
            ca_file: Some(other_ca),
            host_subject: None,
        };
        assert!(try_connect(port, "localhost", &o).await.is_err());
        // The server certificate itself in the CA file is trusted (partial chain).
        let pinned = p.dir.join("leaf.pem");
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64_lines(p.leaf_der.as_ref())
        );
        std::fs::write(&pinned, pem).unwrap();
        let o = TlsOptions {
            ca_file: Some(pinned),
            host_subject: None,
        };
        try_connect(port, "localhost", &o).await.unwrap();
        let _ = std::fs::remove_dir_all(&p.dir);
        let _ = std::fs::remove_dir_all(&other.dir);
    }

    fn base64_lines(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16
                | (*c.get(1).unwrap_or(&0) as u32) << 8
                | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s.as_bytes()
            .chunks(64)
            .map(|l| std::str::from_utf8(l).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn subject_strings() {
        assert_eq!(
            parse_subject("C=GB, O=Example\\, Ltd,CN=host").unwrap(),
            [
                ("C".into(), "GB".into()),
                ("O".into(), "Example, Ltd".into()),
                ("CN".into(), "host".into())
            ]
        );
        assert_eq!(parse_subject("").unwrap(), Vec::<(String, String)>::new());
        assert!(parse_subject("C=").is_none());
        assert!(parse_subject("C=GB,O").is_none());
        assert!(parse_subject("C=GB\\x").is_none());
        assert!(parse_subject(",C=GB").is_none());
    }
}
