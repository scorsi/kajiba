//! ADR 0020 list pagination: opaque base64url cursors holding the method, the sort
//! key and the integer key of the last row, so pages stay stable under inserts.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use tonic::Status;

pub fn encode(method: &str, key: &str, id: i64) -> String {
    URL_SAFE_NO_PAD.encode(format!("{method}\n{key}\n{id}"))
}

/// `None` for the first page; a malformed or foreign cursor is INVALID_ARGUMENT.
pub fn decode(method: &str, token: &str) -> Result<Option<(String, i64)>, Status> {
    if token.is_empty() {
        return Ok(None);
    }
    URL_SAFE_NO_PAD
        .decode(token)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| {
            let (rest, id) = text.rsplit_once('\n')?;
            let (owner, key) = rest.split_once('\n')?;
            (owner == method).then_some((key.to_owned(), id.parse().ok()?))
        })
        .map(Some)
        .ok_or_else(|| invalid_field("page_token", "malformed or foreign cursor"))
}

/// 0 means 50; values above 200 are clamped.
pub fn page_size(size: i32) -> Result<i64, Status> {
    match size {
        0 => Ok(50),
        1.. => Ok(i64::from(size.min(200))),
        _ => Err(invalid_field("page_size", "must not be negative")),
    }
}

fn invalid_field(field: &str, message: &str) -> Status {
    let mut details = tonic_types::ErrorDetails::new();
    details.set_error_info("INVALID_FIELD", "kajiba", std::collections::HashMap::new());
    details.add_bad_request_violation(field, message);
    <Status as tonic_types::StatusExt>::with_error_details(
        tonic::Code::InvalidArgument,
        format!("invalid {field}: {message}"),
        details,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_malformed_or_foreign_cursors() {
        let token = encode("a", "alice/demo", 7);
        assert_eq!(decode("a", &token).unwrap(), Some(("alice/demo".into(), 7)));
        assert_eq!(decode("a", "").unwrap(), None);
        for token in [
            encode("b", "alice/demo", 7),
            "!".into(),
            URL_SAFE_NO_PAD.encode("a\nx\ny"),
        ] {
            let status = decode("a", &token).unwrap_err();
            assert_eq!(status.code(), tonic::Code::InvalidArgument);
        }
        assert_eq!(page_size(0).unwrap(), 50);
        assert_eq!(page_size(500).unwrap(), 200);
        assert_eq!(page_size(3).unwrap(), 3);
        assert!(page_size(-1).is_err());
    }
}
