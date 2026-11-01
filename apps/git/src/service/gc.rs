use super::*;

impl Git {
    pub(super) async fn gc_all(&self) -> Result<i32, Status> {
        let repositories = self.repositories.clone();
        let ids = tokio::task::spawn_blocking(move || repositories.gc_inventory())
            .await
            .map_err(|_| Status::internal("gc inventory worker failed"))??;
        let mut server = server_client(&self.server_socket, &self.generation).await?;
        for id in ids {
            let session = Session {
                session_id: kajiba_core::new_id().map_err(|e| Status::internal(e.to_string()))?,
                kind: SessionKind::Maintenance.into(),
                repo_public_id: id,
                user_public_id: String::new(),
                correlation_id: kajiba_core::new_id()
                    .map_err(|e| Status::internal(e.to_string()))?,
                generation: self.generation.clone(),
            };
            let admitted = async {
                let opened = server
                    .open_session(gate::OpenSessionRequest {
                        user_public_id: String::new(),
                        kind: session.kind,
                        repository: Some(gate::open_session_request::Repository::RepoPublicId(
                            session.repo_public_id.clone(),
                        )),
                        correlation_id: session.correlation_id.clone(),
                        session_id: session.session_id.clone(),
                        generation: session.generation.clone(),
                    })
                    .await?
                    .into_inner();
                if opened.session.as_ref() != Some(&session) {
                    return Err(Status::failed_precondition("maintenance session changed"));
                }
                self.repositories.register(session.clone(), String::new())
            }
            .await;
            if let Err(err) = admitted {
                // An OpenSession reply can be lost after admission; cancel its exact id.
                self.repositories.cancelled(session);
                return Err(err);
            }
            tracing::info!(
                event = "session-started",
                request_id = session.correlation_id,
                session = session.session_id,
                kind = "maintenance",
                repo = session.repo_public_id
            );
            let repositories = self.repositories.clone();
            let id = session.repo_public_id.clone();
            let span = tracing::Span::current();
            // A worker panic is not proof of child exit: leave the session live (0053).
            let result =
                tokio::task::spawn_blocking(move || span.in_scope(|| repositories.gc(&id)))
                    .await
                    .map_err(|_| Status::internal("gc worker failed"))?;
            let status = result.as_ref().copied().unwrap_or(1);
            self.repositories.finished(&session.session_id, status);
            // The monitor retries this fact if the server is down or maintenance delays it.
            if server
                .session_exited(gate::SessionExitedRequest {
                    session_id: session.session_id.clone(),
                    exit_status: status,
                })
                .await
                .is_ok()
            {
                self.repositories.acknowledged(&session.session_id);
            }
            let status = result?;
            if status != 0 {
                return Ok(status);
            }
        }
        Ok(0)
    }
}
