//! The client certificate/key pair, kept as PEMs next to the document.
//!
//! Outside `settings.json` on purpose: a document that fails to parse can fall back to defaults
//! without silently discarding the identity every host has pinned.
use anyhow::{Context, Result};

/// Loads the identity, generating and writing one on first run.
pub fn load_or_create_identity() -> Result<(String, String)> {
    let dir = super::app_dir();
    let (cert_path, key_path) = (dir.join("client-cert.pem"), dir.join("client-key.pem"));
    if let (Ok(cert), Ok(key)) = (std::fs::read_to_string(&cert_path), std::fs::read_to_string(&key_path)) {
        return Ok((cert, key));
    }
    let (cert, key) = punktfunk_core::quic::endpoint::generate_identity().context("generate_identity")?;
    std::fs::write(&cert_path, &cert).context("write client-cert.pem")?;
    std::fs::write(&key_path, &key).context("write client-key.pem")?;
    Ok((cert, key))
}
