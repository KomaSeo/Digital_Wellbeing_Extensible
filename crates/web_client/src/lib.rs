//! Responsibility: Make the HTTPS clients every crate uses, trusting both the certificates the
//! computer trusts and the list built into the program.
//!
//! The computer's list alone is not enough on a freshly installed Windows: Windows keeps only a
//! few root certificates and downloads the others the first time a program built on Windows'
//! own HTTPS asks for them. A program with its own HTTPS (this one) never triggers that
//! download, so GitHub's certificate failed with "UnknownIssuer" on a new PC. The built-in list
//! (Mozilla's, from `webpki-roots`) covers that; the computer's list keeps working behind a
//! company or antivirus program that inspects HTTPS with its own certificate.

use std::sync::{Arc, OnceLock};

/// The TLS settings every client shares, made once.
pub fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            Arc::new(
                rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .expect("ring supports the default TLS versions")
                    .with_root_certificates(root_store())
                    .with_no_client_auth(),
            )
        })
        .clone()
}

/// The certificates a server's chain may end in: the built-in list plus the computer's.
fn root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    match rustls_native_certs::load_native_certs() {
        Ok(certificates) => {
            let (added, ignored) = roots.add_parsable_certificates(certificates);
            tracing::debug!("system certificates: {added} added, {ignored} unreadable");
        }
        Err(error) => tracing::warn!("could not read the system's certificates: {error}"),
    }
    roots
}

/// A client builder that trusts both certificate lists; add a timeout and build it.
pub fn agent_builder() -> ureq::AgentBuilder {
    ureq::AgentBuilder::new().tls_config(tls_config())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub's certificates end in Sectigo's USERTrust roots, which a fresh Windows does not
    /// have until it downloads them; the built-in list must hold them.
    #[test]
    fn trusts_github_without_the_computers_list() {
        let roots = root_store();
        assert!(roots.len() >= webpki_roots::TLS_SERVER_ROOTS.len());
        for name in [
            "USERTrust ECC Certification Authority",
            "USERTrust RSA Certification Authority",
        ] {
            assert!(
                roots.roots.iter().any(|anchor| anchor
                    .subject
                    .as_ref()
                    .windows(name.len())
                    .any(|window| window == name.as_bytes())),
                "{name} missing"
            );
        }
        assert!(Arc::ptr_eq(&tls_config(), &tls_config()));
    }
}
