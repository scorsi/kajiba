use super::super::repos::tests::{DEMO, Fixture, git};
use axum::{body::Body, http};
use tonic::codegen::Service;

async fn get(fixture: &Fixture, path: &str) -> (u16, String) {
    let response = super::super::router(fixture.repos.0.clone(), false, Default::default())
        .unwrap()
        .call(http::Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn raw_serves_files_by_branch_nested_branch_tag_and_commit() {
    let mut fixture = Fixture::new(DEMO).await;
    fixture
        .fill(
            "alice/demo",
            &[
                ("flake.nix", "{ }\n"),
                ("feature/x/README.md", "on main\n"),
                ("docs/a.md", "a\n"),
            ],
            &["refs/tags/v1"],
        )
        .await;
    // Branch `feature/x` holds a different README.md at its root.
    let bare = fixture.dir.join(format!(
        "repos/{}.git",
        crate::db::repos::find(fixture.repos.readers().unwrap(), "alice", "demo")
            .await
            .unwrap()
            .unwrap()
            .public_id
    ));
    let blob = {
        let path = fixture.dir.join("tmp/readme");
        std::fs::write(&path, "on feature/x\n").unwrap();
        let blob = git(&bare, &["hash-object", "-w", path.to_str().unwrap()]);
        std::fs::remove_file(path).unwrap();
        blob
    };
    let mut mktree = std::process::Command::new("git")
        .current_dir(&bare)
        .args(["mktree"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    writeln!(
        mktree.stdin.take().unwrap(),
        "100644 blob {}\tREADME.md",
        blob.trim()
    )
    .unwrap();
    let tree = String::from_utf8(mktree.wait_with_output().unwrap().stdout).unwrap();
    let main = git(&bare, &["rev-parse", "refs/heads/main"]);
    let commit = git(
        &bare,
        &["commit-tree", tree.trim(), "-p", main.trim(), "-m", "x"],
    );
    git(
        &bare,
        &["update-ref", "refs/heads/feature/x", commit.trim()],
    );

    for (path, body) in [
        ("/raw/alice/demo/main/flake.nix", "{ }\n"),
        ("/raw/alice/demo/main/feature/x/README.md", "on main\n"),
        ("/raw/alice/demo/feature/x/README.md", "on feature/x\n"),
        ("/raw/alice/demo/v1/docs/a.md", "a\n"),
        (&format!("/raw/alice/demo/{}/docs/a.md", main.trim()), "a\n"),
        ("/raw/alice/old/main/flake.nix", "{ }\n"),
    ] {
        assert_eq!(get(&fixture, path).await, (200, body.to_owned()), "{path}");
    }
    for (path, status) in [
        ("/raw/alice/demo/main/docs/../flake.nix", 400),
        ("/raw/alice/demo/main//flake.nix", 400),
        ("/raw/alice/demo/main/flake.nix%00", 400),
        ("/raw/alice/demo/main/missing.md", 404),
        ("/raw/alice/demo/main/docs", 404),
        ("/raw/alice/demo/unknown/flake.nix", 404),
        ("/raw/alice/missing/main/flake.nix", 404),
    ] {
        assert_eq!(get(&fixture, path).await.0, status, "{path}");
    }

    // Archived: refused by name and by redirect.
    fixture.apply("").await;
    for path in [
        "/raw/alice/demo/main/flake.nix",
        "/raw/alice/old/main/flake.nix",
    ] {
        let (status, body) = get(&fixture, path).await;
        assert_eq!(status, 403, "{path}");
        assert!(body.contains("archived"));
    }
}

#[tokio::test]
async fn raw_blob_size_limit_and_permit_follow_the_allocation() {
    let fixture = Fixture::new(DEMO).await;
    let limit = super::BLOB_LIMIT as usize;
    let large = "a".repeat(limit + 1);
    fixture
        .fill(
            "alice/demo",
            &[("at-limit", &large[..limit]), ("above-limit", &large)],
            &[],
        )
        .await;
    let mut app = super::super::router(fixture.repos.0.clone(), false, Default::default()).unwrap();
    let response = app
        .call(
            http::Request::get("/raw/alice/demo/main/above-limit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        fixture.repos.0.public_reads.available_permits(),
        super::super::PUBLIC_READS
    );

    let response = app
        .call(
            http::Request::get("/raw/alice/demo/main/at-limit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let bytes = axum::body::to_bytes(response.into_body(), limit)
        .await
        .unwrap();
    assert_eq!(bytes.len(), limit);
    assert_eq!(
        fixture.repos.0.public_reads.available_permits(),
        super::super::PUBLIC_READS - 1
    );
    drop(bytes);
    assert_eq!(
        fixture.repos.0.public_reads.available_permits(),
        super::super::PUBLIC_READS
    );
}
