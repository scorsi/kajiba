use super::repos::State;
use crate::db::permissions::role;
use kajiba_core::config::Role::{self, Admin, Read, Write};

const USERS: &str = "[users.carol]\n[users.dave]\n[users.erin]\nis_admin=true\n[users.frank]\n";

async fn id(state: &mut State, sql: &str, name: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(name)
        .fetch_one(&mut state.writer)
        .await
        .unwrap()
}

async fn roles(state: &mut State, repo: &str) -> Vec<(&'static str, Option<Role>)> {
    let repo = id(state, "SELECT id FROM repo WHERE config_key = ?", repo).await;
    let mut roles = Vec::new();
    for user in ["alice", "bob", "carol", "dave", "erin", "frank"] {
        let user_id = id(state, "SELECT id FROM user WHERE name = ?", user).await;
        roles.push((user, role(&mut state.writer, user_id, repo).await.unwrap()));
    }
    roles
}

#[tokio::test]
async fn owner_admin_permissions_and_public_read_form_the_role_matrix() {
    let mut state = State::new().await;
    let demo = "[repos.demo]\nowner='alice'\ndescription=''\n\
                permissions={bob='write', carol='read', dave='admin'}\n";
    state.apply(&format!("{USERS}{demo}")).await.unwrap();
    assert_eq!(
        roles(&mut state, "demo").await,
        [
            ("alice", Some(Admin)),
            ("bob", Some(Write)),
            ("carol", Some(Read)),
            ("dave", Some(Admin)),
            ("erin", Some(Admin)),
            ("frank", Some(Read)),
        ]
    );
    // An explicit role below the owner's never lowers it.
    state
        .apply(&format!(
            "{USERS}[repos.demo]\nowner='alice'\ndescription=''\npermissions.alice='read'\n"
        ))
        .await
        .unwrap();
    assert_eq!(roles(&mut state, "demo").await[0], ("alice", Some(Admin)));
    state.close().await;
}

#[tokio::test]
async fn removed_permission_falls_back_to_public_read() {
    let mut state = State::new().await;
    let declared =
        |role: &str| format!("{USERS}[repos.demo]\nowner='alice'\ndescription=''\n{role}");
    state
        .apply(&declared("permissions.bob='write'\n"))
        .await
        .unwrap();
    assert_eq!(roles(&mut state, "demo").await[1], ("bob", Some(Write)));
    state.apply(&declared("")).await.unwrap();
    assert_eq!(roles(&mut state, "demo").await[1], ("bob", Some(Read)));
    state.close().await;
}

#[tokio::test]
async fn disabled_users_have_no_role_and_external_users_only_read() {
    let mut state = State::new().await;
    let demo = "[repos.demo]\nowner='alice'\ndescription=''\npermissions.carol='admin'\n";
    state.apply(&format!("{USERS}{demo}")).await.unwrap();
    // carol is removed from the configuration: disabled, her permission row gone.
    state
        .apply(
            "[users.dave]\n[users.erin]\nis_admin=true\n[users.frank]\n\
             [repos.demo]\nowner='alice'\ndescription=''\n",
        )
        .await
        .unwrap();
    assert_eq!(roles(&mut state, "demo").await[2], ("carol", None));
    // External users are never declared: a runtime row with a stray write permission.
    sqlx::query(
        "INSERT INTO user (public_id, name, kind, is_admin, source, created_at)
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAV', 'ext', 'external', 0, 'runtime', 0)",
    )
    .execute(&mut state.writer)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO repo_permission (user_id, repo_id, role)
         SELECT user.id, repo.id, 'admin' FROM user, repo WHERE user.name = 'ext'",
    )
    .execute(&mut state.writer)
    .await
    .unwrap();
    let repo = id(
        &mut state,
        "SELECT id FROM repo WHERE config_key = ?",
        "demo",
    )
    .await;
    let ext = id(&mut state, "SELECT id FROM user WHERE name = ?", "ext").await;
    assert_eq!(
        role(&mut state.writer, ext, repo).await.unwrap(),
        Some(Read)
    );
    // Unknown user or repository: no role.
    assert_eq!(role(&mut state.writer, -1, repo).await.unwrap(), None);
    assert_eq!(role(&mut state.writer, ext, -1).await.unwrap(), None);
    state.close().await;
}

#[tokio::test]
async fn archived_repositories_are_read_only_for_everyone() {
    let mut state = State::new().await;
    let demo = "[repos.demo]\nowner='alice'\ndescription=''\npermissions.bob='write'\n";
    let keep = "[repos.keep]\nowner='bob'\ndescription=''\n";
    state.apply(&format!("{USERS}{demo}{keep}")).await.unwrap();
    state.apply(&format!("{USERS}{keep}")).await.unwrap();
    assert!(state.repo("demo").await.2.is_some());
    let read = ["alice", "bob", "carol", "dave", "erin", "frank"].map(|user| (user, Some(Read)));
    assert_eq!(roles(&mut state, "demo").await, read);
    // Unarchival restores the declared roles.
    state.apply(&format!("{USERS}{demo}{keep}")).await.unwrap();
    assert_eq!(roles(&mut state, "demo").await[1], ("bob", Some(Write)));
    state.close().await;
}
