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
        let (request, facts, refusals) = tokio::task::spawn_blocking(move || {
            let (facts, refusals) = repositories.check_signatures(&request)?;
            Ok::<_, Status>((request, facts, refusals))
        })
        .await
        .map_err(|_| Status::internal("push checks worker failed"))??;
        let mut reasons = std::collections::BTreeMap::<_, Vec<_>>::new();
        for refusal in &refusals {
            reasons
                .entry(refusal.ref_name.as_str())
                .or_default()
                .push(refusal.reason.as_str());
        }
        let response = server_client(&self.server_socket, &self.generation)
            .await?
            .pre_receive(gate::PreReceiveRequest {
                session_id: request.session_id,
                updates: request
                    .updates
                    .into_iter()
                    .zip(facts.fast_forward)
                    .map(|(update, fast_forward)| {
                        let signature_reason = reasons
                            .get(update.ref_name.as_str())
                            .map_or_else(String::new, |reasons| reasons.join("; "));
                        gate::RefUpdate {
                            ref_name: update.ref_name,
                            old_id: update.old_id,
                            new_id: update.new_id,
                            fast_forward: Some(fast_forward),
                            signature: if signature_reason.is_empty() {
                                gate::SignatureVerdict::Verified
                            } else {
                                gate::SignatureVerdict::Refused
                            }
                            .into(),
                            signature_reason,
                        }
                    })
                    .collect(),
            })
            .await?
            .into_inner();
        Ok(Response::new(PreReceiveResponse {
            allowed: response.allowed,
            refusals: response
                .refusals
                .into_iter()
                .map(|refusal| Refusal {
                    ref_name: refusal.ref_name,
                    reason: refusal.reason,
                })
                .collect(),
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
