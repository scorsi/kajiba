//! Where the CLI may send a credential (ADRs 0099, 0104).
use http::Uri;

/// `Ok(true)`: TLS to an `https` URL. `Ok(false)`: plain `http` to `localhost`, `127.0.0.1`
/// or `[::1]`, accepted only in the explicit `development` mode; the caller warns when it
/// uses it. Anything else is refused before a connection is opened.
pub fn credential_tls(url: &str, development: bool) -> Result<bool, String> {
    let uri: Uri = url
        .parse()
        .map_err(|err| format!("invalid URL {url}: {err}"))?;
    if uri.scheme_str() == Some("https") {
        return Ok(true);
    }
    let loopback = uri.scheme_str() == Some("http")
        && uri.host().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
        });
    if loopback && development {
        return Ok(false);
    }
    Err(format!(
        "refusing to send a credential to {url}: the URL must be https \
         (http only to localhost, 127.0.0.1 or [::1], in development mode)"
    ))
}

#[cfg(test)]
mod tests {
    use super::credential_tls;

    #[test]
    fn https_always_loopback_http_only_in_development_mode() {
        for url in [
            "https://forge.example.org",
            "HTTPS://forge.example.org:8443",
        ] {
            assert_eq!(credential_tls(url, false), Ok(true), "{url}");
            assert_eq!(credential_tls(url, true), Ok(true), "{url}");
        }
        for url in [
            "http://localhost:8080",
            "http://LOCALHOST",
            "http://127.0.0.1:1",
            "http://[::1]:8080",
        ] {
            assert_eq!(credential_tls(url, true), Ok(false), "{url}");
            assert!(credential_tls(url, false).is_err(), "{url}");
        }
        for url in [
            "http://forge.example.org",
            "http://127.0.0.2:8080",
            "http://localhost.example.org",
            "http://[::2]",
            "ftp://forge.example.org",
        ] {
            for development in [false, true] {
                assert_eq!(
                    credential_tls(url, development).unwrap_err(),
                    format!(
                        "refusing to send a credential to {url}: the URL must be https \
                         (http only to localhost, 127.0.0.1 or [::1], in development mode)"
                    )
                );
            }
        }
        assert!(
            credential_tls("not a url", true)
                .unwrap_err()
                .starts_with("invalid URL not a url: ")
        );
    }
}
