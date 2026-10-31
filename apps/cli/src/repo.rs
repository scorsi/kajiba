//! `kajiba repo list|get` (0007, 0026): read-only, as the server returns repositories.
use crate::client::{self, Api, Error};
use clap::Subcommand;
use kajiba_proto::v1::{
    CiMode, GetRepoRequest, ListReposRequest, Repo, Role, Visibility, get_repo_request,
    repo_service_client::RepoServiceClient,
};
use prost_reflect::{DescriptorPool, DynamicMessage};
use std::io::Write;

#[derive(Subcommand)]
pub enum Command {
    /// List the repositories you can read, archived ones included.
    List,
    /// Show one repository.
    Get {
        /// `<owner>/<name>` (current or previous) or a public id.
        repo: String,
    },
}

pub async fn run(command: Command, api: Api, json: bool, out: &mut dyn Write) -> Result<(), Error> {
    let mut repos = RepoServiceClient::new(api);
    let text = match command {
        Command::List => {
            let mut all = Vec::new();
            let mut page_token = String::new();
            loop {
                let page = client::answer(
                    repos
                        .list_repos(ListReposRequest {
                            page_size: 0,
                            page_token,
                        })
                        .await,
                )?;
                all.extend(page.repos);
                if page.next_page_token.is_empty() {
                    break;
                }
                page_token = page.next_page_token;
            }
            if json {
                let list = all
                    .iter()
                    .map(|repo| protojson("kajiba.v1.Repo", repo))
                    .collect::<Result<Vec<_>, _>>()?;
                pretty(serde_json::to_string_pretty(&list))?
            } else {
                all.iter().map(line).collect()
            }
        }
        Command::Get { repo } => {
            let repo = if repo.contains('/') {
                get_repo_request::Repo::Name(repo)
            } else {
                get_repo_request::Repo::PublicId(repo)
            };
            let found = client::answer(repos.get_repo(GetRepoRequest { repo: Some(repo) }).await)?
                .repo
                .ok_or_else(|| Error("the forge answered no repository".into()))?;
            if json {
                pretty(serde_json::to_string_pretty(&protojson(
                    "kajiba.v1.Repo",
                    &found,
                )?))?
            } else {
                details(&found)
            }
        }
    };
    out.write_all(text.as_bytes())
        .map_err(|err| Error(format!("stdout: {err}")))
}

/// protojson (camelCase field names, enum names) through the descriptor set.
pub(crate) fn protojson(
    name: &str,
    message: &impl prost::Message,
) -> Result<DynamicMessage, Error> {
    let failed = |err: String| Error(format!("protojson: {err}"));
    let pool = DescriptorPool::decode(kajiba_proto::FILE_DESCRIPTOR_SET)
        .map_err(|err| failed(err.to_string()))?;
    let descriptor = pool
        .get_message_by_name(name)
        .ok_or_else(|| failed(format!("{name} is not in the descriptor set")))?;
    DynamicMessage::decode(descriptor, message.encode_to_vec().as_slice())
        .map_err(|err| failed(err.to_string()))
}

/// Serialized from the messages themselves: a `serde_json::Value` would sort the fields.
pub(crate) fn pretty(json: serde_json::Result<String>) -> Result<String, Error> {
    json.map(|json| json + "\n")
        .map_err(|err| Error(format!("protojson: {err}")))
}

fn line(repo: &Repo) -> String {
    let mut line = format!("{}/{}", repo.owner, repo.name);
    if repo.archived {
        line += " (archived)";
    }
    if !repo.description.is_empty() {
        line = format!("{line}  {}", repo.description);
    }
    line + "\n"
}

/// `VISIBILITY_PUBLIC` → `public`.
fn short(name: &str, prefix: &str) -> String {
    name.strip_prefix(prefix).unwrap_or(name).to_lowercase()
}

fn details(repo: &Repo) -> String {
    let mut text = format!("{}/{}\n", repo.owner, repo.name);
    let mut field = |name: &str, value: String| {
        if !value.is_empty() {
            text += &format!("{name}: {value}\n");
        }
    };
    field("id", repo.public_id.clone());
    field("description", repo.description.clone());
    field(
        "visibility",
        short(
            Visibility::try_from(repo.visibility)
                .unwrap_or_default()
                .as_str_name(),
            "VISIBILITY_",
        ),
    );
    field("default branch", repo.default_branch.clone());
    field(
        "ci",
        short(
            CiMode::try_from(repo.ci_mode)
                .unwrap_or_default()
                .as_str_name(),
            "CI_MODE_",
        ),
    );
    field("archived", if repo.archived { "yes" } else { "" }.into());
    field("previous names", repo.previous_names.join(", "));
    field(
        "permissions",
        repo.permissions
            .iter()
            .map(|permission| {
                let role = Role::try_from(permission.role).unwrap_or_default();
                format!("{} {}", permission.user, short(role.as_str_name(), "ROLE_"))
            })
            .collect::<Vec<_>>()
            .join(", "),
    );
    for mirror in &repo.mirrors {
        let mut value = mirror.remote_url.clone();
        if !mirror.credential.is_empty() {
            value += &format!(" (credential {})", mirror.credential);
        }
        if !mirror.enabled {
            value += " (disabled)";
        }
        field("mirror", value);
    }
    text
}

#[cfg(test)]
mod tests;
