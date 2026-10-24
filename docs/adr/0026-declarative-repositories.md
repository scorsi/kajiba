# ADR 0026 — Declarative repositories, permissions, mirrors, runners and ref rules

Status: Accepted — 2026-10-24

## Context

ADR 0014 made the Nix-generated configuration (0019) the source of truth for users, keys and declared PATs: the server reconciles them at startup and the API refuses to change them. Repositories, permissions, mirrors and runners were still planned as runtime state, created by host-local commands (`admin repo create`, 0021), then by RPCs from J4 (`RepoService.Create`, permission RPCs), with runner rows created on first connection (0008). That leaves two sources of truth for the same deployment: what a repository is, who may write to it and where it is mirrored would live in the database, outside review. The owner extends the declarative scope to them. Each item below names the accepted ADR point it supersedes or completes; everything else in those ADRs stands.

## Decision

### Declarative scope

- **Declared in the configuration and reconciled at startup**, in the same pass as users and credentials (0014), never changed at runtime:
  - **repositories**: existence, owner, name, visibility, default branch, description, `ci_enabled`, and the previous names that keep redirects;
  - **repository permissions**: `read`, `write` or `admin` per declared user;
  - **mirrors**, configuration part: remote URL, credential name, enabled;
  - **runners**: name and declared capabilities;
  - **ref rules** (0013, 0014).
- **Runtime state stays in the database** and is never declared: `webauthn_credential`, `session`, `enrollment_token`, runtime PATs, CI history (`run`, `job`, `job_attempt`, `step`, `log_chunk`, `artifact`), `event`, `handler_checkpoint`, `pending_push`, the mirror queue and status (state, attempts, last success and error), runner connection state (version, last contact). `repo_redirect` is derived from the declared previous names.
- **No RPC, CLI command or UI path mutates a repository, a permission, a mirror configuration, a runner declaration or a ref rule.** The API and the UI show them read-only. There is no `Create`, `Update` or `Delete` method for these resources, so `DECLARED_IN_CONFIG` (0020) is not needed for them; it keeps its meaning for identity (0014).

### Configuration fields (amends 0019)

| Field                            | Default  | Meaning                                                                                              |
| -------------------------------- | -------- | ---------------------------------------------------------------------------------------------------- |
| `repos.<key>`                    |          | a declared repository; `<key>` is its stable identity (below), validated as a name (0021)            |
| `repos.<key>.owner`              | required | a declared user                                                                                      |
| `repos.<key>.name`               | `<key>`  | repository name (0021)                                                                               |
| `repos.<key>.description`        | required | free text                                                                                            |
| `repos.<key>.visibility`         | `public` | `public` only until private repositories exist (0013)                                                |
| `repos.<key>.default_branch`     | `main`   | `HEAD` of the bare repository; deleting it is refused (0014)                                         |
| `repos.<key>.ci_enabled`         | `false`  | 0010                                                                                                 |
| `repos.<key>.previous_names`     | `[]`     | `<owner>/<name>` pairs kept as redirects (0021)                                                      |
| `repos.<key>.permissions.<user>` | none     | `read`, `write` or `admin`; the owner is `admin` implicitly                                          |
| `repos.<key>.mirrors[]`          | `[]`     | `{ remote_url, credential, enabled = true }`; `credential` names a `mirror.credentials` entry (0011) |
| `runners.<name>.capabilities`    | none     | `{ arch, os, backends, tags, max_jobs }` the runner may announce (0008)                              |

`config check` refuses, naming the fields: an owner or permission user that is not declared; two repositories with the same current `<owner>/<name>`; a previous name that is the current name of a declared repository or that appears twice; a mirror credential that is not declared; two mirrors of one repository with the same `remote_url`. Ref rules have no field yet (below).

### Identity across renames

- **The stable identity of a repository is its configuration key**, stored in `repo.config_key`. Reconciliation matches rows by key only, never by name:
  - a key in the configuration and not in the database → **creation**: `git init --bare` in `tmp/`, `HEAD` set to the default branch, `rename` into `repos/<public id>.git`, then the row is committed (0012);
  - a key in both whose owner or name differ → **rename**: the row's `owner_id` and `name` change; `public_id` and the directory do not (0012);
  - a key of an active row and not in the configuration → **archival** (below);
  - a key of an archived row back in the configuration → **unarchival** (below).
- **Redirects of an active repository are exactly its declared `previous_names`.** Reconciliation makes `repo_redirect` equal to them: adding a previous name adds a redirect, removing it removes the redirect. A rename without the old name in `previous_names` leaves no redirect. Redirect resolution itself (HTTP 301 for the UI, transparent for git and SSH) is unchanged (0021).
- Changing a key would archive the old row and create a new one, which the name lock (below) refuses under the same name: a key is permanent. The key is chosen by the deployment and never shown to users.

### Archival and re-declaration

- **A repository removed from the configuration is archived, never deleted.** Reconciliation sets `repo.archived_at` and keeps the row and its git data in place (`repos/<public id>.git` is not moved). Its `public_id`, metadata, CI history (runs, jobs, logs, artifacts until their retention), mirror rows and status, and its events stay readable through the API, the CLI and the UI, which list it and show it as archived and read-only. In the same transaction its permissions are deleted (they are declared and no longer are), its redirects are kept as they were, so links never break, and its queued or running runs are cancelled. Its mirrors stop syncing. `repo-archived{repo}` is logged.
- **An archived repository is not accessible by git.** Clone, fetch and push over smart HTTP and SSH are refused with a message saying that the repository is archived (HTTP 403; an error on the SSH channel). `/raw/` is refused the same way: it is the machine download path, like git transport. The API and the UI keep browsing its code (tree, blob, log, diff), read-only and shown archived.
- **Its names stay locked forever.** Name uniqueness (`<owner>/<name>`) covers the current name and the redirects of every row, archived included: no other repository may take a current or previous name of an archived one. `config check` cannot see the database, so reconciliation refuses a declaration, a rename or a `previous_names` entry that would take a name held by an archived repository, naming both keys, and startup aborts with nothing written. Name resolution finds the row (active or archived) holding the name, then a redirect; a redirect to an archived repository resolves to it (API and UI browsing; git transport and `/raw/` still refused). Lookups by `public_id` always resolve.
- The startup orphan sweep (0032) counts every row, archived included, as registered: quarantine to `tmp/orphans/` applies only to directories that match no row.
- A retained `pending_push` row (unexpected ref value, 0003) does not block archival: the row and the directory stay as they are, so the evidence is kept, and its recovery resumes when the repository is unarchived.
- **Re-declaring the same key unarchives the same row with its content intact**: `archived_at` is cleared, the declared fields are applied (owner and name may differ, as for a rename) and its redirects become the declared `previous_names` again, git access resumes on the same directory, CI history and mirror status are the row's own, and mirrors resume from the declaration. No `admin import` is needed. `repo-unarchived{repo}` is logged.
- **Deleting a repository is out of scope.** No path removes a row or its git data; a full deletion mechanism may come later, in its own ADR.

### Import of content

- **Importing repository content stays a host-local command**, because it is data, not configuration: `kajiba-server admin import <owner>/<name> <source>` (0021: a path to a bare repository, a bundle, or a URL; all branches and tags, history intact).
- The target must be **declared and empty** (no ref at all; an archived repository is not declared); otherwise the command fails with `NOT_FOUND`/`REPO_NOT_FOUND` or `FAILED_PRECONDITION`/`INVALID_STATE`, and nothing is written. The source must contain the declared default branch; otherwise `FAILED_PRECONDITION`/`INVALID_STATE`.
- Only `refs/heads/*` and `refs/tags/*` are imported (0013). The import goes through the **admission gate and the `pending_push` durability of 0003**: the source's refs and their ids are read first, a pending row records them (old = zero id), the exact ids are fetched with `git fetch --atomic`, and reconciliation turns the row into one `push` event per ref. The principal of these events is `system`. Signatures (0014) are not checked: the command runs as root and brings existing history.
- `system` pushes never trigger CI (the trigger rule of 0013 requires a `local` user); the mirror handler treats them like any push, so an enabled mirror whose remote already holds the same refs pushes nothing (0011).

### Runners (amends 0008)

- The `runner` row is created by reconciliation from `runners.<name>`, not on first connection. A runner removed from the configuration has its row disabled (`disabled_at`, kept while job attempts reference it) and its connections refused.
- On connection, the announced capabilities must be within the declared ones (same `arch` and `os`, backends and tags a subset, `max_jobs` at most the declared value); otherwise the connection is refused with `runner-refused{runner,reason="capabilities"}`. Matching (0008) uses the announced capabilities of the live connection.

### Ref rules

- The rules of 0013 and 0014 (allowed prefixes, roles for fast-forward, non-fast-forward and deletion, default branch protected, signatures) are fixed by the product and need no declaration. Per-pattern rules (0014, "prepared for") will be declared under `repos.<key>.ref_rules`, never through the API; their format is fixed when the first one is needed. No table holds them: `pre-receive` reads them from the loaded configuration.

### Amended points

- **0003**: `pending_push.principal_id` may be empty for a `system` principal (import). The admission gate and reconciliation are delivered with the first ref mutation, the import (J2); the hooks join them in J3.
- **0007**: the CLI no longer creates repositories; its repository commands are read-only.
- **0008**: runner rows and capabilities as above.
- **0011**: mirror configuration is declared; `MirrorService` exposes status only.
- **0012**: repositories are created only by startup reconciliation; the deletion path ("delete the row, then the directory") is replaced by an archived row whose directory stays in place. `admin repo list` stays and marks archived repositories.
- **0013**: "Items declared in the configuration (`source = config`)" now covers repositories, permissions, mirror configuration and runners. These tables hold declared rows only, so they get no `source` column; `repo.config_key` identifies a repository.
- **0014**: repository permissions are declared; there are no permission RPCs.
- **0019**: the fields above.
- **0021**: "Creation before API authentication" is replaced: no `admin repo create`, no `RepoService.Create` in J4; a rename is a configuration change; a redirect lasts as long as it is declared; name uniqueness covers the names and redirects of archived repositories too, and an archived repository stays reachable by its `public_id`, its name and its redirects, read-only, browsable through the API and the UI, without git transport or `/raw/`. `admin import` targets a declared, empty repository.
- **0022**: events added: `repo-created{repo}`, `repo-renamed{repo,from}`, `repo-archived{repo}` (warn), `repo-unarchived{repo}`, `repo-imported{repo,refs}`; `runner-refused` gains the reason `capabilities`. `repo` is `<owner>/<name>`.

## Consequences

- Creating, renaming, granting and mirroring are deployment changes, reviewed like users and keys. Restarting the server applies them.
- The UI and the CLI have fewer write paths; J4 ships no repository or permission mutations.
- A configuration mistake that removes a repository loses nothing: the row, its history and its git data stay, and re-declaring the key restores it as it was.
- Every query that admits git access, CI or mirroring filters on `archived_at IS NULL`; listings, lookups and history pages do not.
- A name, once used, is never available to another repository; the disk space of an archived repository is never reclaimed until a deletion mechanism exists.
- `config check` cannot see the database: a rename whose old name is not in `previous_names` is valid and silently drops the old URL. The deployment documents previous names.
- J2 takes over the admission gate from J3 because import is the first ref mutation.

## Rejected alternatives

- Matching a renamed repository by `previous_names` only: two declarations that list the same old name, or an old name removed from the list too early, turn a rename into a delete plus a create.
- Recording redirects automatically on rename: a second, undeclared source of state that the configuration would not show.
- Deleting a removed repository's row: its run, log, artifact and mirror pages and its API lookups would break.
- Quarantining an archived repository's directory: re-declaring the key would need an `admin import` to bring the content back, and `tmp/orphans/` would mix archived repositories with true orphans.
- Freeing an archived repository's name: a URL, a clone or a flake input that named the old repository would silently reach another one.
- Re-declaring a key as a new row: the old history would stay detached from a repository that is, for the deployment, the same one.
- `admin repo create` and `RepoService.Create`: a second source of truth next to the configuration, as for users (0014).
- Import as a configuration field (`repos.<key>.import_from`): content is data; a declarative import would re-run or need its own state.
- Runner rows created on first connection: the API would show runners nobody declared, and capabilities would be whatever a host announces.
