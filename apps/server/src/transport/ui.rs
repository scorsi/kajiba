//! `serve --ui-dir` (ADRs 0094, 0128, 0146): the web application from the bundle the module
//! provides; it is not embedded. Any GET or HEAD no other route answers gets the bundle's file at that
//! path, or `index.html` for a page path, which the SPA routes in the browser.

use axum::{
    body::Body,
    http::{
        HeaderValue, Method, StatusCode,
        header::{CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, VARY},
    },
    response::{IntoResponse, Response},
};
use std::path::{Component, Path};

/// `None` when the request is not one for the bundle: the caller's refusal applies.
/// `accept_encoding` is the request's header: a precompressed `.br` or `.gz` sibling of the
/// file is sent when the client accepts it.
pub(super) async fn file(
    dir: &Path,
    method: &Method,
    path: &str,
    accept_encoding: &str,
) -> Option<Response> {
    if method != Method::GET && method != Method::HEAD {
        return None;
    }
    let relative = Path::new(path.trim_start_matches('/'));
    // Only plain names: no `..`, no root, nothing that leaves the bundle.
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let file = dir.join(relative);
    let (file, served) = if tokio::fs::metadata(&file)
        .await
        .is_ok_and(|meta| meta.is_file())
    {
        (file, relative)
    } else if relative.extension().is_some() {
        // A missing asset is a 404, not the application page.
        return None;
    } else {
        (dir.join("index.html"), Path::new("index.html"))
    };
    let mut encoding = None;
    let mut file = file;
    for (name, suffix) in [("br", "br"), ("gzip", "gz")] {
        let mut variant = file.clone().into_os_string();
        variant.push(format!(".{suffix}"));
        if accepts(accept_encoding, name)
            && tokio::fs::metadata(&variant)
                .await
                .is_ok_and(|meta| meta.is_file())
        {
            file = variant.into();
            encoding = Some(name);
            break;
        }
    }
    let dir = tokio::fs::canonicalize(dir).await.ok()?;
    let file = tokio::fs::canonicalize(file).await.ok()?;
    if !file.starts_with(&dir) {
        return None;
    }
    let Ok(bytes) = tokio::fs::read(&file).await else {
        return Some(StatusCode::NOT_FOUND.into_response());
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    let cache = if served.starts_with("_app/immutable") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut response = (
        [(CONTENT_TYPE, content_type(served)), (CACHE_CONTROL, cache)],
        body,
    )
        .into_response();
    response
        .headers_mut()
        .insert(VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Some(encoding) = encoding {
        response
            .headers_mut()
            .insert(CONTENT_ENCODING, HeaderValue::from_static(encoding));
    }
    Some(response)
}

fn accepts(header: &str, coding: &str) -> bool {
    header.split(',').any(|item| {
        let mut parts = item.split(';').map(str::trim);
        parts.next() == Some(coding) && !parts.any(|param| param == "q=0")
    })
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(dir: &Path, method: Method, path: &str) -> Option<(String, String)> {
        let response = file(dir, &method, path, "").await?;
        assert_eq!(response.status(), StatusCode::OK);
        let kind = response.headers()[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        Some((kind, String::from_utf8(body.to_vec()).unwrap()))
    }

    #[tokio::test]
    async fn serves_files_page_paths_and_nothing_outside() {
        let dir = std::env::temp_dir().join(format!("kajiba-ui-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("_app")).unwrap();
        std::fs::write(dir.join("index.html"), "page").unwrap();
        std::fs::write(dir.join("_app/start.js"), "code").unwrap();
        let html = || Some(("text/html; charset=utf-8".to_owned(), "page".to_owned()));

        assert_eq!(get(&dir, Method::GET, "/").await, html());
        assert_eq!(get(&dir, Method::GET, "/-/login").await, html());
        assert_eq!(
            get(&dir, Method::GET, "/alice/demo/tree/main").await,
            html()
        );
        assert_eq!(
            get(&dir, Method::GET, "/_app/start.js").await,
            Some(("text/javascript; charset=utf-8".into(), "code".into()))
        );
        assert_eq!(
            get(&dir, Method::HEAD, "/-/enroll").await,
            Some(("text/html; charset=utf-8".into(), String::new()))
        );
        assert!(get(&dir, Method::GET, "/_app/missing.js").await.is_none());
        assert!(get(&dir, Method::POST, "/-/login").await.is_none());
        assert!(get(&dir, Method::GET, "/../index.html").await.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn serves_precompressed_variants_and_cache_headers() {
        let dir = std::env::temp_dir().join(format!("kajiba-ui-enc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("_app/immutable")).unwrap();
        std::fs::write(dir.join("index.html"), "page").unwrap();
        std::fs::write(dir.join("index.html.gz"), "page-gz").unwrap();
        std::fs::write(dir.join("_app/immutable/a.js"), "code").unwrap();
        std::fs::write(dir.join("_app/immutable/a.js.br"), "code-br").unwrap();
        std::fs::write(dir.join("_app/immutable/a.js.gz"), "code-gz").unwrap();

        let head = |response: &Response, name: &str| {
            response
                .headers()
                .get(name)
                .map(|value| value.to_str().unwrap().to_owned())
        };
        let get = |path: &'static str, accept: &'static str| {
            let dir = dir.clone();
            async move { file(&dir, &Method::GET, path, accept).await.unwrap() }
        };

        let br = get("/_app/immutable/a.js", "gzip, br").await;
        assert_eq!(head(&br, "content-encoding").as_deref(), Some("br"));
        assert_eq!(
            head(&br, "cache-control").as_deref(),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(head(&br, "vary").as_deref(), Some("Accept-Encoding"));
        let body = axum::body::to_bytes(br.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"code-br");
        let gz = get("/_app/immutable/a.js", "gzip;q=0.8").await;
        assert_eq!(head(&gz, "content-encoding").as_deref(), Some("gzip"));
        let plain = get("/_app/immutable/a.js", "br;q=0, identity").await;
        assert_eq!(head(&plain, "content-encoding"), None);
        let page = get("/-/login", "gzip").await;
        assert_eq!(head(&page, "cache-control").as_deref(), Some("no-cache"));
        assert_eq!(head(&page, "content-encoding").as_deref(), Some("gzip"));
        assert_eq!(
            head(&page, "content-type").as_deref(),
            Some("text/html; charset=utf-8")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refuses_symlinks_outside_the_bundle() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("kajiba-ui-{}", kajiba_core::new_id().unwrap()));
        let dir = root.join("bundle");
        let outside = root.join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(dir.join("index.html"), "page").unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(outside.join("secret.txt"), dir.join("secret.txt")).unwrap();
        symlink(&outside, dir.join("assets")).unwrap();

        for method in [Method::GET, Method::HEAD] {
            for path in ["/secret.txt", "/assets/secret.txt"] {
                assert!(file(&dir, &method, path, "").await.is_none());
            }
        }

        std::fs::remove_file(dir.join("index.html")).unwrap();
        symlink(outside.join("secret.txt"), dir.join("index.html")).unwrap();
        for method in [Method::GET, Method::HEAD] {
            for path in ["/index.html", "/", "/-/login"] {
                assert!(file(&dir, &method, path, "").await.is_none());
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
