use super::{Git, server_client};
use kajiba_proto::{gate::v1 as gate, hook::v1::*};
use tonic::{Request, Response, Status};

#[tonic::async_trait]
impl hook_service_server::HookService for Git {
    async fn get_auth_key(
        &self,
        request: Request<GetAuthKeyRequest>,
    ) -> Result<Response<GetAuthKeyResponse>, Status> {
        let found = server_client(&self.server_socket, &self.generation)
            .await?
            .get_auth_key(gate::GetAuthKeyRequest {
                fingerprint: request.into_inner().fingerprint,
            })
            .await?
            .into_inner();
        Ok(Response::new(GetAuthKeyResponse {
            user_public_id: found.user_public_id,
            public_key: found.public_key,
        }))
    }

    async fn pre_receive(
        &self,
        request: Request<PreReceiveRequest>,
    ) -> Result<Response<PreReceiveResponse>, Status> {
        let request = request.into_inner();
        context(
            &request.user_public_id,
            &request.repo_public_id,
            &request.session_id,
        )?;
        let repositories = self.repositories.clone();
        let (_facts, refusals) =
            tokio::task::spawn_blocking(move || repositories.check_signatures(&request))
                .await
                .map_err(|_| Status::internal("push checks worker failed"))??;
        if !refusals.is_empty() {
            return Ok(Response::new(PreReceiveResponse {
                allowed: false,
                refusals,
            }));
        }
        // Fail closed until durable admission is wired (J3 task 21).
        Ok(Response::new(PreReceiveResponse {
            allowed: false,
            refusals: vec![Refusal {
                ref_name: String::new(),
                reason: "pushes are not accepted yet".into(),
            }],
        }))
    }

    async fn post_receive(
        &self,
        request: Request<PostReceiveRequest>,
    ) -> Result<Response<PostReceiveResponse>, Status> {
        let request = request.into_inner();
        context(
            &request.user_public_id,
            &request.repo_public_id,
            &request.session_id,
        )?;
        server_client(&self.server_socket, &self.generation)
            .await?
            .post_receive(gate::PostReceiveRequest {
                session_id: request.session_id,
            })
            .await?;
        Ok(Response::new(PostReceiveResponse {}))
    }
}

#[allow(clippy::result_large_err)]
fn context(user: &str, repo: &str, session: &str) -> Result<(), Status> {
    for (field, value) in [
        ("user_public_id", user),
        ("repo_public_id", repo),
        ("session_id", session),
    ] {
        if !kajiba_core::names::is_public_id(value) {
            return Err(kajiba_core::local::rpc_error(
                tonic::Code::InvalidArgument,
                "INVALID_FIELD",
                format!("invalid {field}"),
            ));
        }
    }
    Ok(())
}
