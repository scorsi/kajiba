use super::*;
use kajiba_core::local::{Local, effective_uid};
use kajiba_proto::hook::v1::hook_service_server::HookServiceServer;
use std::{
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.org")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.org")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

// The actual thin hook runs in receive-pack's quarantine environment, in a child process.
#[tokio::test]
async fn hook_process() {
    let Some(socket) = std::env::var_os("KAJIBA_TEST_HOOK_SOCKET") else {
        return;
    };
    let kind = match std::env::var("KAJIBA_TEST_HOOK").unwrap().as_str() {
        "pre-receive" => crate::hook::Kind::PreReceive,
        "post-receive" => crate::hook::Kind::PostReceive,
        _ => panic!("unexpected hook"),
    };
    let code = crate::hook::run(Path::new(&socket), kind, std::io::stdin(), &|name| {
        std::env::var(name).ok()
    })
    .await;
    if kind == crate::hook::Kind::PostReceive && code == std::process::ExitCode::SUCCESS {
        eprintln!("post-receive acknowledged");
    }
    std::process::exit(if code == std::process::ExitCode::SUCCESS {
        0
    } else {
        1
    });
}

#[tokio::test]
async fn receive_pack_checks_then_forwards_once_and_never_changes_refs_on_refusal() {
    let temp = TempDir::new();
    let _lock = crate::state::prepare(&temp.0).unwrap();
    let generation = kajiba_core::new_id().unwrap();
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned();
    let repositories = Arc::new(crate::repositories::Repositories::new(temp.0.clone()));
    repositories.create(&id, "main").unwrap();
    let target = temp.0.join(format!("repos/{id}.git"));
    let source = temp.0.join("source.git");
    git(&source, &["init", "--bare"]);
    let key = temp.0.join("key");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );
    let tree = git(&source, &["mktree"]);
    let signing = format!("user.signingKey={}", key.display());
    let signed = git(
        &source,
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &signing,
            "commit-tree",
            &tree,
            "-S",
            "-m",
            "signed",
        ],
    );
    let unsigned = git(
        &source,
        &["commit-tree", &tree, "-p", &signed, "-m", "unsigned"],
    );
    git(
        &source,
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &signing,
            "tag",
            "-s",
            "v1",
            &signed,
            "-m",
            "release",
        ],
    );
    let tag = git(&source, &["rev-parse", "refs/tags/v1"]);
    repositories
        .register(
            Session {
                session_id: id.clone(),
                user_public_id: id.clone(),
                repo_public_id: id.clone(),
                generation: generation.clone(),
                kind: SessionKind::ReceivePack.into(),
                correlation_id: "admission-test".into(),
            },
            format!(
                "{id} {}",
                std::fs::read_to_string(key.with_extension("pub")).unwrap()
            ),
        )
        .unwrap();

    let gate_socket = temp.0.join("gate.sock");
    let hook_socket = temp.0.join("hook.sock");
    let bind =
        |path: &Path| Local::bind(path, Arc::default(), 0o660, vec![effective_uid()]).unwrap();
    let admissions = Arc::new(Mutex::new(Vec::new()));
    let verdict = Arc::new(Mutex::new(gate::PreReceiveResponse {
        allowed: false,
        refusals: vec![gate::Refusal {
            ref_name: "refs/heads/bad".into(),
            reason: format!("commit {unsigned}: unsigned"),
        }],
    }));
    let server = tokio::spawn(bind(&gate_socket).serve(
        Routes::new(gate::gate_service_server::GateServiceServer::new(Server {
            version: Arc::new(Mutex::new(env!("CARGO_PKG_VERSION").into())),
            calls: Arc::default(),
            generation: generation.clone(),
            exit_calls: Arc::default(),
            acknowledge: Arc::default(),
            admissions: admissions.clone(),
            verdict: verdict.clone(),
        })),
        std::future::pending(),
    ));
    let daemon = tokio::spawn(bind(&hook_socket).serve(
        Routes::new(HookServiceServer::new(Git {
            generation,
            server_socket: gate_socket,
            repositories,
        })),
        std::future::pending(),
    ));
    let hooks = temp.0.join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let executable = std::env::current_exe().unwrap();
    for name in ["pre-receive", "post-receive"] {
        assert!(!target.join("hooks").join(name).exists());
        let hook = hooks.join(name);
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\nexport KAJIBA_TEST_HOOK={name}\nexec '{}' --exact service::tests::admission::hook_process --nocapture\n",
                executable.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let push = |specs: &[String]| {
        let mut command = tokio::process::Command::new("git");
        command
            .arg("--git-dir")
            .arg(&source)
            .arg("push")
            .arg(format!(
                "--receive-pack=git -c core.hooksPath='{}' receive-pack",
                hooks.display()
            ))
            .arg(&target)
            .args(specs)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("KAJIBA_USER", &id)
            .env("KAJIBA_REPO", &id)
            .env("KAJIBA_SESSION", &id)
            .env("KAJIBA_TEST_HOOK_SOCKET", &hook_socket);
        async move {
            tokio::time::timeout(Duration::from_secs(30), command.output())
                .await
                .unwrap()
                .unwrap()
        }
    };
    let excessive: Vec<_> = (0..257)
        .map(|i| format!("{signed}:refs/heads/limit-{i}"))
        .collect();
    let output = push(&excessive).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("push verification exceeds 256 updated refs"),
        "{output:?}"
    );
    assert!(admissions.lock().unwrap().is_empty());
    assert!(git(&target, &["for-each-ref"]).is_empty());

    let good = format!("{signed}:refs/heads/main");
    let output = push(&[good.clone(), format!("{unsigned}:refs/heads/bad")]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&unsigned),
        "{output:?}"
    );
    {
        let calls = admissions.lock().unwrap();
        assert_eq!(calls.len(), 1);
        for update in &calls[0].updates {
            if update.ref_name == "refs/heads/bad" {
                assert_eq!(update.signature, gate::SignatureVerdict::Refused as i32);
                assert!(update.signature_reason.contains(&unsigned));
            } else {
                assert_eq!(update.signature, gate::SignatureVerdict::Verified as i32);
                assert!(update.signature_reason.is_empty());
            }
        }
    }
    assert!(git(&target, &["for-each-ref"]).is_empty());

    *verdict.lock().unwrap() = gate::PreReceiveResponse {
        allowed: false,
        refusals: vec![gate::Refusal {
            ref_name: "refs/heads/main".into(),
            reason: "injected policy refusal".into(),
        }],
    };
    let output = push(&[good.clone(), "refs/tags/v1".into()]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("refs/heads/main: injected policy refusal"),
        "{output:?}"
    );
    assert!(git(&target, &["for-each-ref"]).is_empty());
    {
        let calls = admissions.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].session_id, id);
        assert_eq!(
            calls[1]
                .updates
                .iter()
                .map(|u| (u.ref_name.as_str(), u.old_id.as_str(), u.new_id.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "refs/heads/main",
                    "0000000000000000000000000000000000000000",
                    signed.as_str()
                ),
                (
                    "refs/tags/v1",
                    "0000000000000000000000000000000000000000",
                    tag.as_str()
                ),
            ]
        );
        assert!(
            calls[1]
                .updates
                .iter()
                .all(|u| u.fast_forward == Some(false)
                    && u.signature == gate::SignatureVerdict::Verified as i32)
        );
    }
    *verdict.lock().unwrap() = gate::PreReceiveResponse {
        allowed: true,
        refusals: vec![],
    };
    let output = push(&[good, "refs/tags/v1".into()]).await;
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("post-receive acknowledged"),
        "{output:?}"
    );
    assert_eq!(admissions.lock().unwrap().len(), 3);
    assert_eq!(git(&target, &["rev-parse", "refs/heads/main"]), signed);
    assert_eq!(git(&target, &["rev-parse", "refs/tags/v1"]), tag);

    let next = git(
        &source,
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &signing,
            "commit-tree",
            &tree,
            "-p",
            &signed,
            "-S",
            "-m",
            "next",
        ],
    );
    let specs = [
        format!("{next}:refs/heads/main"),
        format!("{next}:refs/heads/topic"),
        ":refs/tags/v1".into(),
    ];
    let output = push(&specs).await;
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("post-receive acknowledged"),
        "{output:?}"
    );
    {
        let calls = admissions.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[3]
                .updates
                .iter()
                .map(|u| (u.ref_name.as_str(), u.fast_forward))
                .collect::<Vec<_>>(),
            [
                ("refs/heads/main", Some(true)),
                ("refs/tags/v1", Some(false)),
                ("refs/heads/topic", Some(false)),
            ]
        );
    }
    let before = git(&target, &["for-each-ref"]);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    let output = push(&[format!("{next}:refs/heads/offline")]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("server unavailable"),
        "{output:?}"
    );
    assert_eq!(git(&target, &["for-each-ref"]), before);
    daemon.abort();
    assert!(daemon.await.unwrap_err().is_cancelled());
    let output = push(&[format!("{next}:refs/heads/offline")]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("git daemon is unavailable"),
        "{output:?}"
    );
    assert_eq!(git(&target, &["for-each-ref"]), before);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("post-receive acknowledged"));
}
