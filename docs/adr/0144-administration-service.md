# ADR 0144 — Administration service

Status: Accepted — 2026-10-31.

## Context

ADR 0139 makes runners, mirrors and server facts visible to forge administrators only, and states that the administration methods require the `admin` scope and the administrator flag. The web application's administration page and the runner summary of its shell need those facts from the server. The host-local `kajiba.admin.v1.AdminService` (0022) serves the host's operator over a socket and is not reachable from a browser.

## Decision

- **A `kajiba.v1.AdministrationService`** (`proto/kajiba/v1/administration.proto`), named so that it does not clash with the local service. Every method has scope `admin` (0014) and, once the interceptor has admitted the credential, answers only a user whose `is_admin` is set, as `GetViewer`'s `admin` does (0081); otherwise `PERMISSION_DENIED` with reason `ADMIN_REQUIRED`. A PAT without the scope is refused by the interceptor as for any method (`SCOPE_MISSING`). A browser session holds every scope (0081).
- **`ListRunners`**: every runner row ordered by name: declared name, architecture, operating system, backends, tags, `max_jobs`, `connected`, `disabled` (removed from the configuration, 0026), the version and hostname announced at the last connection (0107), the last contact and the attempts running now (public id of the run, `<owner>/<name>`, full ref, start of the attempt). An attempt appears only while its state is `running`. The list is not paginated: runners are declared in the configuration and few.
- **`ListMirrors`**: every declared mirror ordered by repository then remote URL: enabled flag, state (`idle`, `queued`, `running`), consecutive failed attempts, last success, last error and its time, next attempt, and the credential's name. The credential's secret is never read. Until a mirror has pushed (J7 fills these columns) `last_success_at` is unset and the web application says "never pushed". Not paginated, for the same reason.
- **`GetServerInfo`**: the server version, its start time, every user with the administrator flag and whether it is disabled, the number of declared tokens that are not revoked, the size of the SQLite file (pages times page size) and of the repositories directory (bytes of the regular files, walked off the async runtime), and the configuration generation of 0141. No token, hash, key or secret value appears in any answer.
- **The web application** reads these three methods through `api.ts` only for an administrator: the forge-wide Activity data, the shell's drawer and the repository page call them only when `GetViewer` says `admin`, so no other user triggers a refusal. A runner removed from the configuration is shown as such and never counted as a fault.

## Consequences

- Additive proto file and service; the new methods are in the correlation table and the scope table like every `kajiba.v1` method.
- The repositories directory is walked at each `GetServerInfo`; the page reads it once per visit. A forge with very large storage may want a cached figure; that is deferred.
- The v2 design's commit of the running build is dropped: the server does not know it.

## Rejected alternatives

- Extending `kajiba.admin.v1.AdminService`: it is the host's socket service, outside the web contract and its authentication.
- Checking only the scope: a PAT with `admin` of a user who is not an administrator would read the forge's machinery, contradicting 0081.
- Letting every user list runners with fewer fields: 0139 decided the fleet is administrators' business.
