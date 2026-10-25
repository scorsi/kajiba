//! Shared domain validation, permissions and events.

pub mod config;
pub mod local;
pub mod log;
pub mod names;
pub mod state;

/// SHA-256 of a secret, the form stored in the database (docs/schema.md).
pub fn sha256(secret: &[u8]) -> [u8; 32] {
    use ssh_key::sha2::{Digest, Sha256};
    Sha256::digest(secret).into()
}

/// Random ULID for correlation and daemon generations.
pub fn new_id() -> std::io::Result<String> {
    use std::{
        io::Read,
        time::{SystemTime, UNIX_EPOCH},
    };
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes[6..])?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_millis();
    let value = (millis << 80) | u128::from_be_bytes(bytes);
    Ok((0..26)
        .rev()
        .map(|shift| {
            b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[((value >> (shift * 5)) & 31) as usize] as char
        })
        .collect())
}
