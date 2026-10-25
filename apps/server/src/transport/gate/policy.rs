use super::{Gate, PreReceiveRequest, RefUpdate, Refusal, SignatureVerdict};
use crate::db::{permissions, pushes::ZERO};
use kajiba_core::{config::Role, names::validate_ref};
use kajiba_proto::git::v1::SessionKind;
use tonic::Status;

impl Gate {
    pub(super) async fn refusals(
        &self,
        writer: &mut sqlx::SqliteConnection,
        request: &PreReceiveRequest,
    ) -> Result<Vec<Refusal>, Status> {
        if request.updates.is_empty() {
            return Ok(vec![Refusal {
                ref_name: String::new(),
                reason: "no ref updates".into(),
            }]);
        }
        let session = self
            .admission
            .session(&request.session_id)
            .filter(|(_, s)| {
                s.kind == SessionKind::ReceivePack as i32
                    && self.admission.generation().as_deref() == Some(s.generation.as_str())
            });
        let Some((repo, session)) = session else {
            return Ok(request
                .updates
                .iter()
                .map(|update| Refusal {
                    ref_name: update.ref_name.clone(),
                    reason: "live receive-pack session required".into(),
                })
                .collect());
        };
        let user: Option<i64> = sqlx::query_scalar("SELECT id FROM user WHERE public_id = ?")
            .bind(&session.user_public_id)
            .fetch_optional(&mut *writer)
            .await
            .map_err(|err| Status::internal(err.to_string()))?;
        let role = match user {
            Some(user) => permissions::role(&mut *writer, user, repo)
                .await
                .map_err(|err| Status::internal(err.to_string()))?,
            None => None,
        };
        let branch: String = sqlx::query_scalar("SELECT default_branch FROM repo WHERE id = ?")
            .bind(repo)
            .fetch_one(&mut *writer)
            .await
            .map_err(|err| Status::internal(err.to_string()))?;
        Ok(request
            .updates
            .iter()
            .filter_map(|update| {
                refusal(update, role, &branch).map(|reason| Refusal {
                    ref_name: update.ref_name.clone(),
                    reason,
                })
            })
            .collect())
    }
}

fn refusal(update: &RefUpdate, role: Option<Role>, default_branch: &str) -> Option<String> {
    if let Err(err) = validate_ref(&update.ref_name) {
        return Some(err.to_string());
    }
    if !update.ref_name.starts_with("refs/heads/") && !update.ref_name.starts_with("refs/tags/") {
        return Some("only refs/heads/* and refs/tags/* may be pushed".into());
    }
    for id in [&update.old_id, &update.new_id] {
        if id.len() != 40 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Some("invalid object id".into());
        }
    }
    if role.is_none_or(|role| role < Role::Write) {
        return Some("active local user with repository write role required".into());
    }
    if update.new_id == ZERO && update.ref_name.strip_prefix("refs/heads/") == Some(default_branch)
    {
        return Some("deleting the default branch is forbidden".into());
    }
    let Some(fast_forward) = update.fast_forward else {
        return Some("fast-forward fact required".into());
    };
    if role != Some(Role::Admin) {
        if update.new_id == ZERO {
            return Some("deleting a ref requires repository admin role".into());
        }
        if update.old_id != ZERO && !fast_forward {
            return Some("non-fast-forward update requires repository admin role".into());
        }
    }
    if update.new_id != ZERO && update.signature != SignatureVerdict::Verified as i32 {
        return Some(
            if update.signature == SignatureVerdict::Refused as i32
                && !update.signature_reason.is_empty()
            {
                format!("signature verification failed: {}", update.signature_reason)
            } else {
                "verified signature verdict required".into()
            },
        );
    }
    None
}

#[cfg(test)]
mod tests;
