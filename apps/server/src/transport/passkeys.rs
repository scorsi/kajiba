//! `PasskeyService` (ADR 0014, 0143): the caller's passkeys, listed and removed from a
//! browser session only; a removal needs a fresh step-up and never takes the last one.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use super::{
    Health,
    auth::{Principal, admit, now},
    cursor,
    local::refused,
    webauthn::STEP_UP_TTL,
};
use crate::db::passkeys::Removal;
use kajiba_proto::{
    google::protobuf::Timestamp,
    v1::{
        DeletePasskeyRequest, DeletePasskeyResponse, ListPasskeysRequest, ListPasskeysResponse,
        Passkey, passkey_service_server::PasskeyService,
    },
};
use tonic::{Request, Response, Status};

pub struct Passkeys(pub Health);

fn internal(err: sqlx::Error) -> Status {
    Status::internal(format!("passkeys: {err}"))
}

fn timestamp(millis: i64) -> Timestamp {
    Timestamp {
        seconds: millis.div_euclid(1000),
        nanos: (millis.rem_euclid(1000) * 1_000_000) as i32,
    }
}

/// The session caller (the interceptor already refused PATs, ADR 0081) and its session hash.
fn session_caller<T>(request: &Request<T>) -> Result<(i64, [u8; 32]), Status> {
    let principal = request
        .extensions()
        .get::<Principal>()
        .ok_or_else(|| Status::internal("passkeys: no authenticated caller"))?;
    let session = principal.session.ok_or_else(|| {
        refused(
            tonic::Code::PermissionDenied,
            "SESSION_REQUIRED",
            "passkeys are managed from a browser session".into(),
        )
    })?;
    Ok((principal.user_id, session))
}

#[tonic::async_trait]
impl PasskeyService for Passkeys {
    async fn list_passkeys(
        &self,
        request: Request<ListPasskeysRequest>,
    ) -> Result<Response<ListPasskeysResponse>, Status> {
        const METHOD: &str = "v1.ListPasskeys";
        let (user, _) = session_caller(&request)?;
        let request = request.into_inner();
        let size = cursor::page_size(request.page_size)?;
        let (_, after) = cursor::decode(METHOD, &request.page_token)?.unwrap_or_default();
        let readers = self
            .0
            .database
            .get()
            .ok_or_else(|| Status::unavailable("starting"))?;
        let mut rows: Vec<(i64, String, String, i64, Option<i64>)> = sqlx::query_as(
            "SELECT id, public_id, name, created_at, last_used_at FROM webauthn_credential
             WHERE user_id = ? AND id > ? ORDER BY id LIMIT ?",
        )
        .bind(user)
        .bind(after)
        .bind(size + 1)
        .fetch_all(readers)
        .await
        .map_err(internal)?;
        let more = rows.len() as i64 > size;
        rows.truncate(size as usize);
        let next_page_token = match rows.last() {
            Some((id, ..)) if more => cursor::encode(METHOD, "", *id),
            _ => String::new(),
        };
        Ok(Response::new(ListPasskeysResponse {
            passkeys: rows
                .into_iter()
                .map(|(_, public_id, name, created, used)| Passkey {
                    public_id,
                    name,
                    created_at: Some(timestamp(created)),
                    last_used_at: used.map(timestamp),
                })
                .collect(),
            next_page_token,
        }))
    }

    async fn delete_passkey(
        &self,
        request: Request<DeletePasskeyRequest>,
    ) -> Result<Response<DeletePasskeyResponse>, Status> {
        let (user, hash) = session_caller(&request)?;
        let public_id = request.into_inner().public_id;
        let mut writer = self
            .0
            .writer
            .get()
            .ok_or_else(|| Status::unavailable("starting"))?
            .lock()
            .await;
        let now = now()?;
        // Logout or recovery may have ended the session since authentication read it.
        let session = crate::db::sessions::find(&mut *writer, &hash)
            .await
            .map_err(internal)?
            .filter(|found| found.user_id == user);
        let session = admit(session, now).map_err(|refusal| refusal.status())?;
        let fresh = crate::db::sessions::step_up_fresh(
            &mut *writer,
            session.id,
            STEP_UP_TTL.as_millis() as i64,
            now,
        )
        .await
        .map_err(internal)?;
        if !fresh {
            return Err(refused(
                tonic::Code::PermissionDenied,
                "STEP_UP_REQUIRED",
                "confirm with one of your passkeys first".into(),
            ));
        }
        match crate::db::passkeys::remove(&mut writer, user, &public_id)
            .await
            .map_err(internal)?
        {
            Removal::Removed => Ok(Response::new(DeletePasskeyResponse {})),
            Removal::NotFound => Err(refused(
                tonic::Code::NotFound,
                "PASSKEY_NOT_FOUND",
                "no passkey of yours by this id".into(),
            )),
            Removal::Last => Err(refused(
                tonic::Code::FailedPrecondition,
                "LAST_PASSKEY",
                "your last passkey cannot be removed".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests;
