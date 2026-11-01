//! Linux jobs live in independent system units; the socket broker fixes all privileges (0156).

use crate::{
    durable::{self, Shared, State},
    session::Attempts,
};
use kajiba_exec::{Executor, Exit};
use kajiba_proto::runner::v1::{StepState, StepStatus, connect_request::Message};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{mpsc, oneshot},
};

const SOCKET: &str = "/run/kajiba-job/control";

pub async fn control(operation: &str, id: &str) -> Result<(), String> {
    if !kajiba_core::names::is_public_id(id) {
        return Err("invalid attempt id".into());
    }
    let mut socket = UnixStream::connect(SOCKET)
        .await
        .map_err(|e| format!("job broker: {e}"))?;
    socket
        .write_all(format!("{operation} {id}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    socket.shutdown().await.map_err(|e| e.to_string())?;
    let mut reply = String::new();
    socket
        .take(4096)
        .read_to_string(&mut reply)
        .await
        .map_err(|e| e.to_string())?;
    if reply.trim() != "ok" {
        return Err(format!("job broker: {reply}"));
    }
    Ok(())
}

async fn exists(state: &State) -> Result<bool, String> {
    let output = tokio::process::Command::new("systemctl")
        .args(["show", "--property=LoadState", "--value", &state.unit])
        .output()
        .await
        .map_err(|e| format!("systemctl: {e}"))?;
    let load = String::from_utf8_lossy(&output.stdout);
    match load.trim() {
        "loaded" => Ok(true),
        "not-found" => Ok(false),
        _ => Err(format!(
            "cannot inspect job unit: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Install all held ids before the first hello, including completed units awaiting a result ack.
pub async fn adopt(root: &Path, attempts: &Attempts) -> Result<Vec<Shared>, String> {
    let dir = root.join("jobs");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut adopted = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.ends_with(".done") || !entry.path().join("state.json").exists() {
            // An interrupted creation/cleanup never owns an executable job record.
            fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
            continue;
        }
        let state = State::load(root, &id)?;
        let result = state.dir().join("result.json");
        if !result.exists() && !exists(&state).await? && !result.exists() {
            // No unit and no trustworthy terminal status: omit it, so the forge loses/replays it.
            state.remove();
            continue;
        }
        let shared = Arc::new(Mutex::new(state));
        attempts.adopt(shared.clone())?;
        adopted.push(shared);
    }
    Ok(adopted)
}

pub fn status(attempts: &Attempts, state: &State, value: StepState, code: Option<i32>) {
    attempts.send(Message::StepStatus(StepStatus {
        attempt_id: state.attempt.clone(),
        step_index: 0,
        name: state.step.clone(),
        state: value.into(),
        exit_code: code,
    }));
}

/// Tail from the last indexed boundary; pending chunks are already restored by Attempts::adopt.
pub async fn follow(
    state: Shared,
    attempts: &Attempts,
    cancel: oneshot::Receiver<()>,
) -> Result<(Exit, Option<String>), String> {
    let snapshot = state.lock().unwrap().clone();
    let id = &snapshot.attempt;
    let outcome = tail(state, attempts, cancel).await;
    if outcome.is_err() {
        if let Err(err) = control("stop", id).await {
            // A monitor error cannot report failure while an unobserved unit still runs.
            tracing::error!(event = "job-stop-failed", err);
            std::process::exit(1);
        }
        status(attempts, &snapshot, StepState::Failed, None);
    }
    outcome
}

async fn tail(
    state: Shared,
    attempts: &Attempts,
    mut cancel: oneshot::Receiver<()>,
) -> Result<(Exit, Option<String>), String> {
    let snapshot = state.lock().unwrap().clone();
    status(attempts, &snapshot, StepState::Running, None);
    let mut file = fs::File::open(&snapshot.log).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(snapshot.offset()))
        .map_err(|e| e.to_string())?;
    let mut inspect = tokio::time::Instant::now();
    loop {
        let finished = snapshot.dir().join("result.json");
        // Read the terminal marker before EOF: ExecStopPost publishes it after stdout closes.
        let done = finished.exists();
        let mut data = vec![0; kajiba_exec::CHUNK];
        let size = file.read(&mut data).map_err(|e| e.to_string())?;
        if size > 0 {
            data.truncate(size);
            tokio::select! {
                () = attempts.log(&snapshot.attempt, 0, data) => {},
                _ = &mut cancel => { control("stop", &snapshot.attempt).await?; snapshot.remove(); return Ok((Exit::Cancelled, None)); }
            }
            continue;
        }
        if done {
            let (result, code, value): (String, String, String) =
                serde_json::from_slice(&fs::read(&finished).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let exit = match (result.as_str(), code.as_str(), value.parse::<i32>()) {
                ("success", "exited", Ok(0)) => Exit::Code(0),
                (_, "exited", Ok(code)) if code != 0 => Exit::Code(code),
                (_, "killed" | "dumped", Ok(signal)) => Exit::Signal(signal),
                _ => return Err(format!("job unit: {result} ({code} {value})")),
            };
            let code = if let Exit::Code(code) = exit {
                Some(code)
            } else {
                None
            };
            status(
                attempts,
                &snapshot,
                if exit == Exit::Code(0) {
                    StepState::Succeeded
                } else {
                    StepState::Failed
                },
                code,
            );
            let check = fs::read_to_string(snapshot.dir().join("failing-check")).ok();
            return Ok((exit, check));
        }
        if inspect.elapsed() >= Duration::from_secs(1) {
            if !exists(&snapshot).await? && !finished.exists() {
                return Err("job unit disappeared without a terminal status".into());
            }
            inspect = tokio::time::Instant::now();
        }
        tokio::select! {
            _ = &mut cancel => { control("stop", &snapshot.attempt).await?; snapshot.remove(); return Ok((Exit::Cancelled, None)); }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

/// Fixed ExecStart of the transient unit. It uses the existing backend and its validation.
pub async fn execute(root: &Path, id: &str) -> Result<Exit, String> {
    let state = State::load(root, id)?;
    let executor = kajiba_exec::Nix::default();
    let (env, _) = executor.prepare(&kajiba_exec::Job {
        source: &state.source,
        sha: &state.sha,
        system: &state.system,
        checks: &state.checks,
    })?;
    let (log, mut output) = mpsc::channel::<Vec<u8>>(4);
    // Open as the runner user, never through systemd's privileged stdout-file handling.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&state.log)
        .map_err(|e| e.to_string())?;
    let forward = async {
        let stdout = &mut file;
        while let Some(data) = output.recv().await {
            stdout.write_all(&data).map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(())
    };
    let (exit, output) = tokio::join!(executor.run(&env, 0, log, std::future::pending()), forward);
    output?;
    if let Some(check) = executor.failing_check(&env) {
        durable::atomic(&state.dir().join("failing-check"), check.as_bytes())?;
    }
    executor.cleanup(env);
    exit
}

/// ExecStopPost's service environment records systemd's result even if ExecStart was killed.
pub fn record(root: &Path, id: &str) -> Result<(), String> {
    let state = State::load(root, id)?;
    let values = ["SERVICE_RESULT", "EXIT_CODE", "EXIT_STATUS"]
        .map(|key| std::env::var(key).unwrap_or_default());
    durable::atomic(
        &state.dir().join("result.json"),
        &serde_json::to_vec(&values).map_err(|e| e.to_string())?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kajiba_proto::runner::v1::{Assignment, ConnectRequest};

    #[tokio::test]
    async fn a_collected_finished_unit_is_adopted_and_tailed_from_its_indexed_offset() {
        let root =
            std::env::temp_dir().join(format!("kajiba-unit-finished-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let id = "01K00000000000000000000002";
        let assignment = Assignment {
            attempt_id: id.into(),
            ..Default::default()
        };
        let mut state = State::new(&root, &assignment, root.join("scratch").join(id)).unwrap();
        fs::write(&state.log, b"old\nnew\n").unwrap();
        state.index(4).unwrap();
        state.acknowledge(1).unwrap();
        durable::atomic(
            &state.dir().join("result.json"),
            br#"["success","exited","0"]"#,
        )
        .unwrap();
        let incomplete = root.join("jobs/01K00000000000000000000003");
        let removed = root.join("jobs/01K00000000000000000000004.done");
        fs::create_dir_all(&incomplete).unwrap();
        fs::create_dir_all(&removed).unwrap();
        fs::write(removed.join("state.json"), "incomplete cleanup").unwrap();
        let attempts = Attempts::default();
        let recovered = adopt(&root, &attempts).await.unwrap();
        assert!(!incomplete.exists() && !removed.exists());
        assert_eq!(recovered.len(), 1);
        let (outbound, mut output) = mpsc::unbounded_channel::<ConnectRequest>();
        attempts.attach(Some(outbound));
        let (_cancel, cancelled) = oneshot::channel();
        assert_eq!(
            follow(recovered[0].clone(), &attempts, cancelled)
                .await
                .unwrap(),
            (Exit::Code(0), None)
        );
        let sent: Vec<_> = std::iter::from_fn(|| output.try_recv().ok())
            .map(|r| r.message.unwrap())
            .collect();
        assert!(
            matches!(&sent[0], Message::StepStatus(step) if step.state() == StepState::Running)
        );
        assert!(
            matches!(&sent[1], Message::LogChunk(chunk) if chunk.sequence == 2 && chunk.data == b"new\n")
        );
        assert!(
            matches!(&sent[2], Message::StepStatus(step) if step.state() == StepState::Succeeded)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
