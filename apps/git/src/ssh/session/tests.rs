use super::*;
use std::os::unix::net::UnixStream as BlockingStream;
use tokio::net::UnixStream;

fn pipes() -> ([OwnedFd; 3], [UnixStream; 3]) {
    let pairs: Vec<_> = (0..3)
        .map(|_| {
            let (peer, passed) = BlockingStream::pair().unwrap();
            peer.set_nonblocking(true).unwrap();
            (OwnedFd::from(passed), UnixStream::from_std(peer).unwrap())
        })
        .collect();
    let (fds, peers): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    (fds.try_into().unwrap(), peers.try_into().unwrap())
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

#[tokio::test]
async fn idle_and_blocked_output_kill_and_reap_the_child() {
    for script in [
        "exec sleep 30",
        "exec yes",
        "exec 0<&- 1>&- 2>&-; exec sleep 30",
    ] {
        let (fds, _unread) = pipes();
        let session_info = Default::default();
        let mut exited = true;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run(
                &mut shell(script),
                fds,
                &session_info,
                Duration::from_millis(100),
                Duration::from_secs(30),
                &mut exited,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(result.kind(), io::ErrorKind::TimedOut, "{script}");
        assert!(exited, "child must be reaped before exit is reported");
    }
}

#[tokio::test]
async fn absolute_deadline_expires_despite_continuous_delivery() {
    let (fds, [_input, mut output, _error]) = pipes();
    let session_info = Default::default();
    let mut exited = true;
    let mut command = shell("exec yes");
    let session = run(
        &mut command,
        fds,
        &session_info,
        Duration::from_secs(10),
        Duration::from_millis(150),
        &mut exited,
    );
    let drain = async {
        tokio::io::copy(&mut output, &mut tokio::io::sink())
            .await
            .unwrap()
    };
    let (result, bytes) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(session, drain)
    })
    .await
    .unwrap();
    assert!(bytes > 0);
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(exited);
}

#[tokio::test]
async fn normal_exit_delivers_both_outputs_and_preserves_status() {
    let (fds, [mut input, mut output, mut error]) = pipes();
    let session_info = Default::default();
    let mut exited = true;
    let mut command = shell("cat; printf diagnostic >&2; exit 7");
    let session = run(
        &mut command,
        fds,
        &session_info,
        Duration::from_secs(2),
        Duration::from_secs(5),
        &mut exited,
    );
    let client = async {
        input.write_all(b"request").await.unwrap();
        input.shutdown().await.unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        tokio::try_join!(
            output.read_to_end(&mut stdout),
            error.read_to_end(&mut stderr)
        )
        .unwrap();
        assert_eq!(stdout, b"request");
        assert_eq!(stderr, b"diagnostic");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(session, client)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap().code(), Some(7));
    assert!(exited);
}

#[tokio::test]
async fn input_activity_refreshes_idle_deadline() {
    let (fds, [mut input, mut output, _error]) = pipes();
    let session_info = Default::default();
    let mut exited = true;
    let mut command = shell("cat >/dev/null; printf done");
    let session = run(
        &mut command,
        fds,
        &session_info,
        Duration::from_millis(300),
        Duration::from_secs(5),
        &mut exited,
    );
    let client = async {
        for _ in 0..8 {
            input.write_all(b"input").await.unwrap();
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        input.shutdown().await.unwrap();
        let mut stdout = Vec::new();
        output.read_to_end(&mut stdout).await.unwrap();
        assert_eq!(stdout, b"done");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(session, client)
    })
    .await
    .unwrap();
    assert!(result.unwrap().success());
    assert!(exited);
}

#[tokio::test]
async fn descendant_holding_pipes_cannot_outlive_deadline() {
    let (fds, [_input, mut output, _error]) = pipes();
    let session_info = Default::default();
    let mut exited = true;
    let mut command = shell("sleep 30 & printf '%s\\n' $!; exit 0");
    let session = run(
        &mut command,
        fds,
        &session_info,
        Duration::from_millis(150),
        Duration::from_secs(5),
        &mut exited,
    );
    let client = async {
        let mut stdout = String::new();
        output.read_to_string(&mut stdout).await.unwrap();
        stdout.trim().parse::<libc::pid_t>().unwrap()
    };
    let (result, pid) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(session, client)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(exited);
    // The descendant closed its pipes; allow the OS to reap the orphan after group kill.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // SAFETY: signal zero only checks whether this process still exists.
            if unsafe { libc::kill(pid, 0) } == -1 {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
