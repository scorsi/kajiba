//! Shared domain validation, permissions and events.

pub mod config;
pub mod log;
pub mod names;

/// SHA-256 of a secret, the form stored in the database (docs/schema.md).
pub fn sha256(secret: &[u8]) -> [u8; 32] {
    use ssh_key::sha2::{Digest, Sha256};
    Sha256::digest(secret).into()
}
