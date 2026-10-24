# ADR 0027 — GitHub mirror target preparation

Status: Accepted — 2026-10-24

## Context

ADR 0011 leaves the GitHub side of a mirror to the operator in v0: the repository is created by hand, its settings set by hand, pull requests ignored, automation "later". Today that work is done outside the forge, for example by koseki, an OpenTofu registry of repositories: it creates each GitHub repository, aligns description, visibility and archived, and disables issues, wiki, projects, discussions, Actions and pull requests. ADR 0026 makes mirrors part of kajiba's declared configuration, so the GitHub side can be declared in the same place. kajiba prepares its GitHub targets itself. Each item below names the accepted ADR point it supersedes or completes; everything else in those ADRs stands.

## Decision

### Target preparation (amends 0011, milestone J7)

- A declared mirror (0026) with a **`github` table** targets GitHub; its `remote_url` must be `https://github.com/<owner>/<name>.git` (`config check`). For such a mirror kajiba **ensures the target exists and matches the declaration**:
  - creates the repository if it does not exist, under `<owner>` (a user or an organization);
  - description: `github.description`, or the repository's declared description when absent; homepage: the repository's forge URL (0011);
  - `visibility` (`public` | `private`) and `archived` as declared, in both directions;
  - issues, wiki, projects and discussions disabled; **pull requests disabled**; **Actions disabled**.
- **It never deletes, renames or transfers anything on GitHub.** A forge rename (0026) does not touch the target; pointing a mirror elsewhere is a new `remote_url`. A mirror removed from the configuration, or the mirrors of an archived repository (0026), leave their GitHub repository as it is.
- `archived = true` with `enabled = true` is refused by `config check`: GitHub refuses pushes to an archived repository. An archived target is prepared, never pushed.
- **When**: preparation is the first step of a mirror job when the declaration's settings differ from the last successful preparation (`mirror.prepared_digest`), and of every reconciliation pass (0011), which corrects drift made by hand on GitHub. It runs for disabled mirrors too. It reads the target and writes only the differences. A failure is a mirror error (status, `last_error`, backoff, `kajiba_mirror_syncs_total{result="error"}`), and the job does not push.
- **Adoption**: an existing GitHub repository is adopted as is. Preparation aligns its settings, and the first push changes nothing when its refs already match (0011).

### Credentials

- `github.admin_credential` names a `mirror.credentials` entry used for preparation. It may be the push credential (`credential`). A distinct one is recommended so that the push token never holds administration rights.
- Token permissions (fine-grained PAT, _All repositories_ so that a new mirror needs no token change):
  - preparation: **Administration: read and write** (create, settings, Actions permissions), **Metadata: read**;
  - push: **Contents: read and write**, **Metadata: read**.
- To verify in J7, against GitHub's documentation and a real token before the J7 code relies on them:
  - that a fine-grained PAT with Administration write can create a repository for a user account and in an organization, and which endpoint each case uses;
  - that disabling pull requests (`has_pull_requests`) is accepted by the REST API with that permission;
  - that the Actions permissions endpoint needs Administration write and nothing else;
  - whether pushing history that changes `.github/workflows/` needs **Workflows: read and write** on the push token, even with Actions disabled.
- The GitHub App of 0011 (J15) replaces both tokens later. The HTTP client used for the GitHub API is a J7 dependency decision, recorded in the dependency register (0034).

### Configuration fields (amends 0019)

| Field                          | Default          | Meaning                                    |
| ------------------------------ | ---------------- | ------------------------------------------ |
| `repos.<key>.mirrors[].github` | absent           | the target is GitHub and is prepared       |
| `….github.visibility`          | `public`         | `public` or `private`                      |
| `….github.archived`            | `false`          | refused with `enabled = true`              |
| `….github.description`         | the repository's | description on GitHub                      |
| `….github.admin_credential`    | required         | `mirror.credentials` entry for preparation |

### Amended points

- **0011**: "On the GitHub side, in v0: repo created by hand … Pull requests cannot be disabled on GitHub: ignored in v0. Automation of creation and settings through the API later" is replaced by target preparation, in J7. "Description and website field point to the forge" stands, as the defaults above.
- **0012 / docs/schema.md**: `mirror` gains `prepared_digest` and `prepared_at`.
- **0019**: the fields above.
- **0022**: event added: `mirror-prepared{mirror,changes}` (`changes`: names of the settings written, `created` when the repository was created); failures are `mirror-sync{result="error"}`.

## Consequences

- A new mirrored repository is one declaration: the GitHub repository appears and is configured at the next mirror job.
- Settings changed by hand on GitHub are reverted at the next reconciliation pass.
- The deployment holds one more secret when the admin credential is distinct, and tracks its expiry like the push token's (0011).
- GitHub must never have two writers: a GitHub repository that another tool still manages gets no `github` table.
- kajiba now covers what koseki does, its forge side through ADR 0026 and its GitHub side through this ADR. A deployment that uses koseki can therefore migrate its entries and stop using it at the end of J10 (import from an existing forge), keeping it authoritative until then. Migration note, per koseki entry: `description` → `repos.<key>.description`; the forge side → a declared repository (visibility `public`; an entry that needs a private forge repository waits for private repositories, 0013); `github.mirror` → a mirror with `remote_url = https://github.com/<owner>/<name>.git`; `github.visibility` and `github.archived` → its `github` table; a description suffix added on GitHub → `github.description`. An entry with a GitHub side and no forge side has no kajiba equivalent. Its resources are released from koseki's state without being destroyed, and kajiba adopts the GitHub repositories (above).

## Rejected alternatives

- Leaving the GitHub side to an external tool (such as koseki): two declarations of the same mirror, in two repositories, kept in sync by hand.
- Deleting or renaming GitHub repositories to follow the declaration: a mistake would destroy the public copy that serves as the fallback source (0015).
- Preparing the target before every push: API calls on every sync for settings that rarely change; the digest and the reconciliation pass cover both declaration changes and drift.
- A single token with Administration and Contents always: allowed, but not imposed, because it gives the push path rights it does not need.
