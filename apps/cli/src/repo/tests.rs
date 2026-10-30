use super::*;
use crate::client::{
    connect,
    tests::{Fake, TOKEN},
};
use kajiba_proto::v1::{Mirror, Permission};

fn repos() -> Vec<Repo> {
    vec![
        Repo {
            public_id: "01JBDEMO000000000000000000".into(),
            owner: "alice".into(),
            name: "demo".into(),
            description: "Demo repository".into(),
            visibility: Visibility::Public.into(),
            default_branch: "main".into(),
            ci_enabled: true,
            permissions: vec![Permission {
                user: "bob".into(),
                role: Role::Write.into(),
            }],
            mirrors: vec![Mirror {
                remote_url: "https://example.org/demo.git".into(),
                credential: "upstream".into(),
                enabled: false,
            }],
            ..Default::default()
        },
        Repo {
            public_id: "01JBOLD0000000000000000000".into(),
            owner: "alice".into(),
            name: "old".into(),
            visibility: Visibility::Public.into(),
            default_branch: "main".into(),
            previous_names: vec!["alice/older".into()],
            archived: true,
            ..Default::default()
        },
        Repo {
            public_id: "01JBTOOLS00000000000000000".into(),
            owner: "bob".into(),
            name: "tools".into(),
            visibility: Visibility::Public.into(),
            default_branch: "trunk".into(),
            ..Default::default()
        },
    ]
}

async fn fake() -> (Fake, String) {
    let fake = Fake::default();
    {
        let mut state = fake.0.lock().unwrap();
        state.repos = repos();
        state.page = 1;
    }
    let url = fake.serve().await;
    (fake, url)
}

async fn output(url: &str, command: Command, json: bool) -> Result<String, Error> {
    let mut out = Vec::new();
    run(command, connect(url, TOKEN, true).unwrap(), json, &mut out).await?;
    Ok(String::from_utf8(out).unwrap())
}

#[tokio::test]
async fn list_follows_every_page() {
    let (fake, url) = fake().await;
    assert_eq!(
        output(&url, Command::List, false).await.unwrap(),
        "alice/demo  Demo repository\nalice/old (archived)\nbob/tools\n"
    );
    let calls = &fake.0.lock().unwrap().calls;
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|(method, _)| *method == "ListRepos"));
}

#[tokio::test]
async fn list_json_is_protojson() {
    let (_, url) = fake().await;
    let json = output(&url, Command::List, true).await.unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 3);
    assert_eq!(
        value[1],
        serde_json::json!({
            "publicId": "01JBOLD0000000000000000000",
            "owner": "alice",
            "name": "old",
            "visibility": "VISIBILITY_PUBLIC",
            "defaultBranch": "main",
            "previousNames": ["alice/older"],
            "archived": true
        })
    );
}

#[tokio::test]
async fn get_text_and_json() {
    let (_, url) = fake().await;
    let get = |repo: &str| Command::Get { repo: repo.into() };
    let text = "alice/demo\n\
        id: 01JBDEMO000000000000000000\n\
        description: Demo repository\n\
        visibility: public\n\
        default branch: main\n\
        ci: enabled\n\
        permissions: bob write\n\
        mirror: https://example.org/demo.git (credential upstream) (disabled)\n";
    assert_eq!(output(&url, get("alice/demo"), false).await.unwrap(), text);
    assert_eq!(
        output(&url, get("01JBDEMO000000000000000000"), false)
            .await
            .unwrap(),
        text
    );
    assert_eq!(
        output(&url, get("alice/demo"), true).await.unwrap(),
        r#"{
  "publicId": "01JBDEMO000000000000000000",
  "owner": "alice",
  "name": "demo",
  "description": "Demo repository",
  "visibility": "VISIBILITY_PUBLIC",
  "defaultBranch": "main",
  "ciEnabled": true,
  "permissions": [
    {
      "user": "bob",
      "role": "ROLE_WRITE"
    }
  ],
  "mirrors": [
    {
      "remoteUrl": "https://example.org/demo.git",
      "credential": "upstream"
    }
  ]
}
"#
    );
}

#[tokio::test]
async fn archived_and_redirected_repositories_as_the_server_returns_them() {
    let (_, url) = fake().await;
    // A previous name answers the repository under its current name.
    assert_eq!(
        output(
            &url,
            Command::Get {
                repo: "alice/older".into()
            },
            false
        )
        .await
        .unwrap(),
        "alice/old\n\
         id: 01JBOLD0000000000000000000\n\
         visibility: public\n\
         default branch: main\n\
         ci: disabled\n\
         archived: yes\n\
         previous names: alice/older\n"
    );
}

#[tokio::test]
async fn an_unknown_repository_names_the_reason() {
    let (_, url) = fake().await;
    let err = output(
        &url,
        Command::Get {
            repo: "alice/missing".into(),
        },
        false,
    )
    .await
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "no repository by this id or name (REPO_NOT_FOUND, request 01JBREQUEST0000000000000000)"
    );
}
