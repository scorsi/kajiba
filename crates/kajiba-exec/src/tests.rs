use super::*;
use std::{path::PathBuf, sync::OnceLock, time::Duration};

const SCRIPTS: [(&str, &str); 5] = [
    (
        "succeeds",
        "printf 'args:'; printf ' %s' \"$@\"; echo; echo warning >&2; exit 0",
    ),
    ("fails", "echo 'error: check failed' >&2; exit 3"),
    // About 300 KB of numbered lines, more than four chunks.
    (
        "verbose",
        "i=0; while [ $i -lt 3000 ]; do echo \"line $i ................................................................................\"; i=$((i+1)); done",
    ),
    // Fails only the build of check `b`.
    (
        "fails-b",
        "printf 'args:'; printf ' %s' \"$@\"; echo; case \"$*\" in *.b) exit 4;; esac",
    ),
    // A background child that would leave a marker if it outlived the cancellation.
    (
        "hangs",
        "(sleep 1; echo alive > \"$MARKER\") & echo started; wait",
    ),
];

/// The fake `nix` programs, all written before any test spawns one: a script written
/// while another test forks can fail to exec (`ETXTBSY`).
fn scripts() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("kajiba-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in SCRIPTS {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        dir
    })
}

/// Run the `nix` backend with the fake `program` on the whole flake; returns the exit and
/// the log chunks.
async fn check(
    program: &str,
    cancel: impl Future<Output = ()> + Send,
) -> (Result<Exit, String>, Vec<Vec<u8>>) {
    let (exit, chunks, failed) = build(program, &[], cancel).await;
    assert_eq!(failed, None);
    (exit, chunks)
}

/// Run the `nix` backend with the fake `program` and the selected `checks`; returns the
/// exit, the log chunks and the failing check.
async fn build(
    program: &str,
    checks: &[&str],
    cancel: impl Future<Output = ()> + Send,
) -> (Result<Exit, String>, Vec<Vec<u8>>, Option<String>) {
    let checks: Vec<String> = checks.iter().map(|name| name.to_string()).collect();
    let nix = Nix {
        program: scripts().join(program),
    };
    let (env, steps) = nix
        .prepare(&Job {
            source: Path::new("/scratch/01"),
            sha: "0123456789abcdef0123456789abcdef01234567",
            system: "x86_64-linux",
            checks: &checks,
        })
        .unwrap();
    let step = if checks.is_empty() {
        "flake-check"
    } else {
        "build-checks"
    };
    assert_eq!(steps, [step]);
    let (log, mut received) = mpsc::channel(4);
    let collect = async {
        let mut chunks = Vec::new();
        while let Some(chunk) = received.recv().await {
            chunks.push(chunk);
        }
        chunks
    };
    let (exit, chunks) = tokio::join!(nix.run(&env, 0, log, cancel), collect);
    let failed = nix.failing_check(&env);
    nix.cleanup(env);
    (exit, chunks, failed)
}

#[tokio::test]
async fn success_runs_flake_check_on_the_commit_with_both_streams_logged() {
    let (exit, chunks) = check("succeeds", std::future::pending()).await;
    assert_eq!(exit, Ok(Exit::Code(0)));
    assert_eq!(
        String::from_utf8(chunks.concat()).unwrap(),
        "args: flake check --print-build-logs \
         git+file:///scratch/01?rev=0123456789abcdef0123456789abcdef01234567\nwarning\n"
    );
}

const FLAKE: &str = "git+file:///scratch/01?rev=0123456789abcdef0123456789abcdef01234567";

#[tokio::test]
async fn selected_checks_are_built_one_by_one_and_only_them() {
    let (exit, chunks, failed) = build("succeeds", &["a", "c"], std::future::pending()).await;
    assert_eq!(exit, Ok(Exit::Code(0)));
    assert_eq!(failed, None);
    assert_eq!(
        String::from_utf8(chunks.concat()).unwrap(),
        format!(
            "args: build --no-link --print-build-logs {FLAKE}#checks.x86_64-linux.a\nwarning\n\
             args: build --no-link --print-build-logs {FLAKE}#checks.x86_64-linux.c\nwarning\n"
        )
    );
}

#[tokio::test]
async fn the_first_failing_selected_check_stops_the_step_and_is_named() {
    let (exit, chunks, failed) = build("fails-b", &["a", "b", "c"], std::future::pending()).await;
    assert_eq!(exit, Ok(Exit::Code(4)));
    assert_eq!(failed.as_deref(), Some("b"));
    let text = String::from_utf8(chunks.concat()).unwrap();
    assert!(text.contains("checks.x86_64-linux.b"), "{text}");
    assert!(!text.contains("checks.x86_64-linux.c"), "{text}");
}

#[tokio::test]
async fn failure_reports_the_exit_code_and_the_output() {
    let (exit, chunks) = check("fails", std::future::pending()).await;
    assert_eq!(exit, Ok(Exit::Code(3)));
    assert_eq!(chunks.concat(), b"error: check failed\n");
}

#[tokio::test]
async fn output_arrives_in_order_in_chunks_of_at_most_64_kib() {
    let (exit, chunks) = check("verbose", std::future::pending()).await;
    assert_eq!(exit, Ok(Exit::Code(0)));
    assert!(chunks.iter().all(|chunk| chunk.len() <= CHUNK));
    let text = String::from_utf8(chunks.concat()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3000);
    for (n, line) in lines.iter().enumerate() {
        assert!(line.starts_with(&format!("line {n} ")), "{line}");
    }
}

#[tokio::test]
async fn cancellation_kills_the_whole_process_group() {
    let marker = std::env::temp_dir().join(format!("kajiba-exec-marker-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let nix = Nix {
        program: scripts().join("hangs"),
    };
    let mut command = Command::new(&nix.program);
    command.env("MARKER", &marker);
    let (log, mut received) = mpsc::channel(4);
    let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
    let run = tokio::spawn(spawn(command, log, async {
        let _ = cancelled.await;
    }));
    assert_eq!(received.recv().await.unwrap(), b"started\n");
    cancel.send(()).unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit, Ok(Exit::Cancelled));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !marker.exists(),
        "a process of the group outlived the cancellation"
    );
}

#[tokio::test]
async fn a_missing_program_is_an_error() {
    let nix = Nix {
        program: "/nonexistent/nix".into(),
    };
    let (log, _received) = mpsc::channel(1);
    let (flake, _) = nix
        .prepare(&Job {
            source: Path::new("/scratch/01"),
            sha: "0123456789abcdef0123456789abcdef01234567",
            system: "x86_64-linux",
            checks: &[],
        })
        .unwrap();
    let exit = nix.run(&flake, 0, log, std::future::pending()).await;
    assert!(exit.unwrap_err().starts_with("/nonexistent/nix: "));
}
