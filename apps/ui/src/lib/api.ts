/**
 * Data access for the whole UI — the ONLY module views use to get data.
 *
 * Mirrors `kajiba.v1` with protobuf-es naming (camelCase fields, `{ … }` request
 * objects, numeric enums). Sign-in, enrollment, logout, `getViewer` and token
 * management talk to kajiba-server (the WebAuthn ceremonies and the committed
 * connect-web client); everything else is still a MOCK reading `./mock.ts`, replaced
 * slice by slice in J5 keeping the exported names and types.
 *
 * Everything under "ASSUMED" does not exist in the API yet: each item is a
 * question for the integrator (see DESIGN.md → "Mock assumptions").
 *
 * Dev switch: `?mock=slow|error|unauth|empty|member|broken` forces a state on the mocked calls.
 */

import { createClient } from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";
import { AuthService } from "./gen/kajiba/v1/auth_pb.ts";
import { Scope } from "./gen/kajiba/v1/options_pb.ts";
import {
  RepoService,
  type Commit as CommitMessage,
  type Repo as RepoMessage,
} from "./gen/kajiba/v1/repo_pb.ts";
import {
  TokenService,
  TokenSource,
  type Token as TokenMessage,
} from "./gen/kajiba/v1/token_pb.ts";
import {
  MOCK_MIRROR_STATUS,
  MOCK_PASSKEYS,
  MOCK_TOKENS,
  MOCK_REPOS,
  MOCK_SERVER,
  MOCK_RUNNERS,
  MOCK_RUNS,
  MOCK_DISK_BYTES,
  CHECKS,
  mockRunLog,
} from "./mock";

// ── Types (shape of protobuf-es generated messages) ─────────────────────

/** Connect/gRPC status codes used by the UI (same numbers as `@connectrpc/connect` `Code`). */
export enum Code {
  Unknown = 2,
  NotFound = 5,
  PermissionDenied = 7,
  ResourceExhausted = 8,
  FailedPrecondition = 9,
  Unavailable = 14,
  Unauthenticated = 16,
}

/** Stand-in for `ConnectError` (which also exposes `.code`). */
export class ApiError extends Error {
  constructor(
    public readonly code: Code,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** google.protobuf.Timestamp as protobuf-es v2 generates it. */
export interface Timestamp {
  seconds: bigint;
  nanos: number;
}

export enum Role {
  UNSPECIFIED = 0,
  READ = 1,
  WRITE = 2,
  ADMIN = 3,
}

/** When the repository's CI runs (ADR 0133). */
export enum CiMode {
  UNSPECIFIED = 0,
  OFF = 1,
  MANUAL = 2,
  PUSH = 3,
}

/** The proto's numbers (`kajiba.v1.EntryKind`). */
export enum EntryKind {
  UNSPECIFIED = 0,
  TREE = 1,
  BLOB = 2,
  EXECUTABLE = 3,
  LINK = 4,
  SUBMODULE = 5,
}

/** The proto's numbers (`kajiba.v1.ChangeKind`); renames are not detected. */
export enum ChangeKind {
  UNSPECIFIED = 0,
  ADDED = 1,
  DELETED = 2,
  MODIFIED = 3,
}

export interface Permission {
  user: string;
  role: Role;
}

export interface Mirror {
  remoteUrl: string;
  credential: string;
  enabled: boolean;
}

export interface Repo {
  publicId: string;
  owner: string;
  name: string;
  description: string;
  /** Full ref, e.g. `refs/heads/main` (the server sends the short name). */
  defaultBranch: string;
  ciMode: CiMode;
  previousNames: string[];
  permissions: Permission[];
  mirrors: Mirror[];
  archived: boolean;
  /** As the server advertises them; absent for an archived repository. */
  cloneUrls?: { https: string; ssh: string };
}

export interface TreeEntry {
  name: string;
  kind: EntryKind;
  id: string;
  /** Bytes of a file; absent for directories, links and submodules. */
  size?: number;
}

export interface Commit {
  id: string;
  parentIds: string[];
  authorName: string;
  authorEmail: string;
  commitTime?: Timestamp;
  message: string;
}

export interface Change {
  path: string;
  kind: ChangeKind;
  oldId: string;
  newId: string;
}

/** Repository reference used in requests: `"<owner>/<name>"` or a public id. */
export type RepoRef = string;

// ── RepoService ─────────────────────────────────────────────────────────

// Same origin: the session cookie (HttpOnly) rides along; CORS admits only the forge origin.
const transport = createGrpcWebTransport({
  baseUrl: typeof location === "undefined" ? "/" : location.origin,
});
const repos = createClient(RepoService, transport);

/** The server takes a short branch or tag name (or a commit id), not a full ref. */
const short = (revision: string) =>
  revision.replace(/^refs\/(heads|tags)\//, "");

function toRepo(m: RepoMessage): Repo {
  return {
    publicId: m.publicId,
    owner: m.owner,
    name: m.name,
    description: m.description,
    defaultBranch: `refs/heads/${m.defaultBranch}`,
    ciMode: m.ciMode as number as CiMode,
    previousNames: m.previousNames,
    permissions: m.permissions.map((p) => ({
      user: p.user,
      role: p.role as number as Role,
    })),
    mirrors: m.mirrors.map((x) => ({
      remoteUrl: x.remoteUrl,
      credential: x.credential,
      enabled: x.enabled,
    })),
    archived: m.archived,
    cloneUrls: m.cloneUrls && {
      https: m.cloneUrls.https,
      ssh: m.cloneUrls.ssh,
    },
  };
}

/** `RepoService.GetRepo` by `<owner>/<name>`, current or previous. */
export async function getRepo(req: { name: string }): Promise<Repo> {
  const { repo } = await repos.getRepo({
    repo: { case: "name", value: req.name },
  });
  if (!repo) throw new ApiError(Code.Unknown, "empty GetRepo answer");
  return toRepo(repo);
}

export async function listRepos(
  req: { pageSize?: number; pageToken?: string } = {},
) {
  const res = await repos.listRepos({
    pageSize: req.pageSize ?? 50,
    pageToken: req.pageToken ?? "",
  });
  return { repos: res.repos.map(toRepo), nextPageToken: res.nextPageToken };
}

export async function getTree(req: {
  repo: RepoRef;
  revision: string;
  path: string;
}) {
  const { commitId, entries } = await repos.getTree({
    ...req,
    revision: short(req.revision),
  });
  return {
    commitId,
    entries: entries.map<TreeEntry>((e) => ({
      name: e.name,
      kind: e.kind as number as EntryKind,
      id: e.id,
      size: e.size === undefined ? undefined : Number(e.size),
    })),
  };
}

export async function getBlob(req: {
  repo: RepoRef;
  revision: string;
  path: string;
}) {
  const { commitId, content } = await repos.getBlob({
    ...req,
    revision: short(req.revision),
  });
  return { commitId, content };
}

export async function listCommits(req: {
  repo: RepoRef;
  revision: string;
  pageSize?: number;
  pageToken?: string;
  /** ASSUMED (J5 task 3): only commits touching this path; ignored until then. */
  path?: string;
}) {
  const res = await repos.listCommits({
    repo: req.repo,
    revision: short(req.revision),
    pageSize: req.pageSize ?? 50,
    pageToken: req.pageToken ?? "",
  });
  return {
    commits: res.commits.map(toCommit),
    nextPageToken: res.nextPageToken,
  };
}

function toCommit(c: CommitMessage): Commit {
  return {
    id: c.id,
    parentIds: c.parentIds,
    authorName: c.authorName,
    authorEmail: c.authorEmail,
    commitTime: c.commitTime && {
      seconds: c.commitTime.seconds,
      nanos: c.commitTime.nanos,
    },
    message: c.message,
  };
}

export async function getCommitDiff(req: { repo: RepoRef; revision: string }) {
  const { changes } = await repos.getCommitDiff({
    ...req,
    revision: short(req.revision),
  });
  return {
    changes: changes.map<Change>((c) => ({
      path: c.path,
      kind: c.kind as number as ChangeKind,
      oldId: c.oldId,
      newId: c.newId,
    })),
  };
}

export interface Ref {
  /** Full name: `refs/heads/main`, `refs/tags/v1.0.0`. */
  name: string;
  /** The commit it resolves to (an annotated tag is peeled). */
  commitId: string;
}

/** Branches and tags, sorted by name. */
export async function listRefs(req: { repo: RepoRef }) {
  const { refs } = await repos.listRefs(req);
  return {
    refs: refs.map<Ref>((r) => ({ name: r.name, commitId: r.commitId })),
  };
}

export interface Viewer {
  user: string;
  /** Forge administrator (sees Administration). */
  admin: boolean;
}

const auth = createClient(AuthService, transport);

/** `AuthService.GetViewer` — who is signed in. Throws `Unauthenticated` otherwise (ADR 0087). */
export async function getViewer(): Promise<Viewer> {
  const { user, admin } = await auth.getViewer({});
  return { user, admin };
}

/** `AuthService.Logout` — ends the browser session and clears its cookie (ADR 0079). */
export async function logout(): Promise<void> {
  await auth.logout({});
}

export enum RunnerState {
  UNSPECIFIED = 0,
  IDLE = 1,
  BUSY = 2,
  OFFLINE = 3,
}

export interface Runner {
  /** Declared runner name (infra config). */
  name: string;
  host: string;
  /** Execution backend: `nix`, `nspawn`, `microvm`. */
  backend: string;
  labels: string[];
  state: RunnerState;
  lastSeenTime?: Timestamp;
  currentRun?: {
    id: string;
    repo: RepoRef;
    ref: string;
    startTime?: Timestamp;
  };
}

/** ASSUMED `ListRunners` (runner service) — declared runners and their live state. */
export async function listRunners() {
  await mockGate();
  const STATE = {
    idle: RunnerState.IDLE,
    busy: RunnerState.BUSY,
    offline: RunnerState.OFFLINE,
  };
  const ts = (s: number): Timestamp => ({ seconds: BigInt(s), nanos: 0 });
  const runners: Runner[] = MOCK_RUNNERS.map((r) => ({
    name: r.name,
    host: r.host,
    backend: r.backend,
    labels: [...r.labels],
    state: STATE[r.state],
    lastSeenTime: ts(r.lastSeen),
    currentRun: r.currentRun && {
      id: r.currentRun.id,
      repo: r.currentRun.repo,
      ref: r.currentRun.ref,
      startTime: ts(r.currentRun.startedAt),
    },
  }));
  return { runners };
}

export enum MirrorState {
  UNSPECIFIED = 0,
  OK = 1,
  FAILING = 2,
  DISABLED = 3,
}

export interface MirrorStatus {
  repo: RepoRef;
  remoteUrl: string;
  state: MirrorState;
  lastAttemptTime?: Timestamp;
  lastSuccessTime?: Timestamp;
  lastError: string;
  /** Consecutive failed attempts (retries with backoff). */
  attempts: number;
  nextAttemptTime?: Timestamp;
}

/** ASSUMED `ListMirrorStatus` — push-mirror outcomes (ADR: retry with alerting). */
export async function listMirrorStatus() {
  await mockGate();
  const STATE = {
    ok: MirrorState.OK,
    failing: MirrorState.FAILING,
    disabled: MirrorState.DISABLED,
  };
  const ts = (s?: number): Timestamp | undefined =>
    s === undefined ? undefined : { seconds: BigInt(s), nanos: 0 };
  const mirrors: MirrorStatus[] = MOCK_REPOS.flatMap((r) =>
    r.mirrors.map((m) => {
      const key = `${r.owner}/${r.name}`;
      const st = MOCK_MIRROR_STATUS[`${key} ${m.remoteUrl}`] ?? {
        state: "disabled" as const,
      };
      return {
        repo: key,
        remoteUrl: m.remoteUrl,
        state: STATE[st.state],
        lastAttemptTime: ts(st.lastAttempt),
        lastSuccessTime: ts(st.lastSuccess),
        lastError: st.error ?? "",
        attempts: st.attempts ?? 0,
        nextAttemptTime: ts(st.nextAttempt),
      };
    }),
  );
  return { mirrors };
}

export interface ServerInfo {
  version: string;
  commit: string;
  startTime?: Timestamp;
  /** Identifier of the Nix configuration generation that produced the running config. */
  configGeneration: string;
  databaseBytes: number;
  storageBytes: number;
  declaredTokens: number;
  users: string[];
}

/** ASSUMED `GetServerInfo` — admin-only overview. */
export async function getServerInfo(): Promise<ServerInfo> {
  await mockGate();
  return {
    version: MOCK_SERVER.version,
    commit: MOCK_SERVER.commit,
    startTime: { seconds: BigInt(MOCK_SERVER.startedAt), nanos: 0 },
    configGeneration: MOCK_SERVER.configGeneration,
    databaseBytes: MOCK_SERVER.databaseBytes,
    storageBytes: MOCK_DISK_BYTES,
    declaredTokens: MOCK_SERVER.declaredTokens,
    users: [...MOCK_SERVER.users],
  };
}

// ── ASSUMED: auth / account service ─────────────────────────────────────

export interface Passkey {
  id: string;
  /** Label chosen at enrolment (authenticator name). */
  name: string;
  createTime?: Timestamp;
  lastUsedTime?: Timestamp;
}

/** Token scopes, as `kajiba.v1.Scope` and the server configuration name them (ADR 0014). */
export const TOKEN_SCOPES = [
  { id: "repo:read", value: Scope.REPO_READ, label: "Read repositories" },
  { id: "repo:write", value: Scope.REPO_WRITE, label: "Push to repositories" },
  { id: "ci:trigger", value: Scope.CI_TRIGGER, label: "Trigger CI runs" },
  { id: "admin", value: Scope.ADMIN, label: "Administer your repositories" },
] as const;

export interface Token {
  id: string;
  name: string;
  scopes: string[];
  createTime?: Timestamp;
  /** Unset = never expires. */
  expireTime?: Timestamp;
  lastUsedTime?: Timestamp;
  /** Declared in the server configuration: shown, never revocable here. */
  declared: boolean;
  revokedTime?: Timestamp;
}

const ts = (s?: number): Timestamp | undefined =>
  s === undefined ? undefined : { seconds: BigInt(s), nanos: 0 };
const nowSec = () => Math.floor(Date.now() / 1000);

/** ASSUMED `ListPasskeys`. */
export async function listPasskeys() {
  await mockGate();
  const passkeys: Passkey[] = MOCK_PASSKEYS.map((p) => ({
    id: p.id,
    name: p.name,
    createTime: ts(p.created),
    lastUsedTime: ts(p.lastUsed),
  }));
  return { passkeys };
}

/**
 * ASSUMED `BeginPasskeyEnrollment` + `navigator.credentials.create` +
 * `FinishPasskeyEnrollment`. `replaceId` re-enrols (replaces) an existing one.
 */
export async function enrollPasskey(req: {
  name: string;
  replaceId?: string;
}): Promise<Passkey> {
  await mockGate();
  await new Promise((r) => setTimeout(r, 900)); // the authenticator prompt
  const pk = {
    id: `pk_${Date.now()}`,
    name: req.name,
    created: nowSec(),
    lastUsed: nowSec(),
  };
  const i = MOCK_PASSKEYS.findIndex((p) => p.id === req.replaceId);
  if (i >= 0) MOCK_PASSKEYS.splice(i, 1, pk);
  else MOCK_PASSKEYS.push(pk);
  return {
    id: pk.id,
    name: pk.name,
    createTime: ts(pk.created),
    lastUsedTime: ts(pk.lastUsed),
  };
}

const tokens = createClient(TokenService, transport);

function toToken(t: TokenMessage): Token {
  return {
    id: t.publicId,
    name: t.name,
    scopes: t.scopes.map(
      (s) => TOKEN_SCOPES.find((scope) => scope.value === s)!.id,
    ),
    createTime: t.createdAt,
    expireTime: t.expiresAt,
    lastUsedTime: t.lastUsedAt,
    declared: t.source === TokenSource.CONFIG,
    revokedTime: t.revokedAt,
  };
}

/** `ListTokens` — all pages, including declared and revoked tokens. */
export async function listTokens() {
  const listed: Token[] = [];
  let pageToken = "";
  do {
    const page = await tokens.listTokens({ pageToken });
    listed.push(...page.tokens.map(toToken));
    pageToken = page.nextPageToken;
  } while (pageToken);
  return { tokens: listed };
}

/** `CreateToken` — the server returns the secret once, never again. */
export async function createToken(req: {
  name: string;
  scopes: string[];
  expireTime: Timestamp;
}): Promise<{ token: Token; secret: string }> {
  const { token, secret } = await tokens.createToken({
    name: req.name,
    scopes: req.scopes.map(
      (s) => TOKEN_SCOPES.find((scope) => scope.id === s)!.value,
    ),
    expiresAt: req.expireTime,
  });
  if (!token) throw new Error("The server returned no token.");
  return { token: toToken(token), secret };
}

/** `RevokeToken` — runtime tokens only; declared tokens are read-only. */
export async function revokeToken(req: { id: string }) {
  return tokens.revokeToken({ publicId: req.id });
}

// ── WebAuthn ceremonies (plain JSON routes, ADRs 0084 and 0085) ─────────

/** Why the server refused a ceremony step (`{"reason"}`), in words. */
const REFUSALS: Record<string, string> = {
  ENROLLMENT_INVALID:
    "This enrollment link has expired, was already used or was replaced.",
  CEREMONY_INVALID: "The request took too long. Reload the page and try again.",
  VERIFICATION_FAILED: "The passkey could not be verified.",
  PASSKEY_INVALID: "This passkey is not recognised.",
  NAME_INVALID: "The passkey name is too long or contains control characters.",
  CEREMONIES_FULL:
    "Too many sign-ins are in progress. Try again in a few minutes.",
  BUSY: "The server is busy. Try again in a moment.",
};

async function ceremony(path: string, body?: unknown): Promise<Response> {
  const res = await fetch(`/auth/webauthn/${path}`, {
    method: "POST",
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.ok) return res;
  const reason = await res
    .json()
    .then((r: { reason?: string }) => r.reason ?? "")
    .catch(() => "");
  throw new ApiError(
    res.status === 503 ? Code.Unavailable : Code.PermissionDenied,
    REFUSALS[reason] ?? `kajiba-server answered ${res.status}.`,
  );
}

/** The browser's prompt, with its refusals (cancelled, timed out) in words. */
async function prompt<T>(run: () => Promise<T | null>): Promise<T> {
  try {
    const credential = await run();
    if (credential) return credential;
  } catch (e) {
    if (!(e instanceof DOMException && e.name === "NotAllowedError")) throw e;
  }
  throw new ApiError(
    Code.PermissionDenied,
    "The passkey prompt was cancelled or timed out.",
  );
}

/** Discoverable login: start → `navigator.credentials.get` → finish, which sets the session. */
export async function signInWithPasskey(): Promise<Viewer> {
  const started = (await (await ceremony("login/start")).json()) as {
    ceremony: string;
    options: { publicKey: PublicKeyCredentialRequestOptionsJSON };
  };
  const publicKey = PublicKeyCredential.parseRequestOptionsFromJSON(
    started.options.publicKey,
  );
  const credential = await prompt(() =>
    navigator.credentials.get({ publicKey }),
  );
  await ceremony("login/finish", {
    ceremony: started.ceremony,
    credential: (credential as PublicKeyCredential).toJSON(),
  });
  return getViewer();
}

/** An enrollment ceremony started from a link: whom it enrolls, and the creation options. */
export interface Enrollment {
  user: string;
  ceremony: string;
  publicKey: PublicKeyCredentialCreationOptionsJSON;
}

/**
 * Checks the link's token and starts its ceremony (5 minutes to finish). Started on page
 * load so that the button opens the prompt directly from the click.
 */
export async function getEnrollment(req: {
  token: string;
}): Promise<Enrollment> {
  const started = (await (
    await ceremony("enroll/start", { token: req.token })
  ).json()) as {
    ceremony: string;
    options: { publicKey: PublicKeyCredentialCreationOptionsJSON };
  };
  const { publicKey } = started.options;
  return { user: publicKey.user.name, ceremony: started.ceremony, publicKey };
}

/** Registers the first passkey of a started enrollment; consumes the link and signs in. */
export async function completeEnrollment(req: {
  enrollment: Enrollment;
  name: string;
}): Promise<Viewer> {
  const publicKey = PublicKeyCredential.parseCreationOptionsFromJSON(
    req.enrollment.publicKey,
  );
  const credential = await prompt(() =>
    navigator.credentials.create({ publicKey }),
  );
  await ceremony("enroll/finish", {
    ceremony: req.enrollment.ceremony,
    credential: (credential as PublicKeyCredential).toJSON(),
    name: req.name,
  });
  return getViewer();
}

// ── ASSUMED: CI runs ────────────────────────────────────────────────────

export enum RunState {
  UNSPECIFIED = 0,
  QUEUED = 1,
  RUNNING = 2,
  SUCCEEDED = 3,
  FAILED = 4,
  CANCELLED = 5,
}

export interface RunStep {
  name: string;
  state: RunState;
  durationSeconds: number;
  /** ASSUMED: sub-steps known in advance (the flake checks a `nix flake check` step builds, from evaluation). */
  checks?: string[];
}

export interface Run {
  id: string;
  repo: RepoRef;
  /** Full ref the run was triggered for. */
  ref: string;
  commitId: string;
  state: RunState;
  createTime?: Timestamp;
  /** Elapsed so far while running. */
  durationSeconds: number;
  runner: string;
  trigger: "push" | "manual";
  steps: RunStep[];
}

const RUN_STATE = {
  queued: RunState.QUEUED,
  running: RunState.RUNNING,
  succeeded: RunState.SUCCEEDED,
  failed: RunState.FAILED,
  cancelled: RunState.CANCELLED,
} as const;

function toRun(r: (typeof MOCK_RUNS)[number]): Run {
  return {
    id: r.id,
    repo: r.repo,
    ref: r.ref,
    commitId: r.commitId,
    state: RUN_STATE[r.state],
    createTime: { seconds: BigInt(r.created), nanos: 0 },
    durationSeconds: r.duration,
    runner: r.runner,
    trigger: r.trigger,
    steps: r.steps.map((s, i) => ({
      name: s.name,
      state: RUN_STATE[s.state],
      durationSeconds: s.duration,
      checks:
        i === 2 && r.steps[1].state === "succeeded" ? [...CHECKS] : undefined,
    })),
  };
}

/** ASSUMED `ListRuns` — newest first, optionally for one repository and ref. */
export async function listRuns(
  req: { repo?: RepoRef; ref?: string; pageSize?: number } = {},
) {
  await mockGate();
  const runs: Run[] = MOCK_RUNS.filter(
    (r) =>
      (!req.repo || r.repo === req.repo) && (!req.ref || r.ref === req.ref),
  )
    .sort((a, b) => b.created - a.created)
    .slice(0, req.pageSize ?? 50)
    .map(toRun);
  return { runs };
}

/** ASSUMED `GetRun` — one run by id. NotFound if unknown or not visible. */
export async function getRun(req: { id: string }): Promise<Run> {
  await mockGate();
  const r = MOCK_RUNS.find((x) => x.id === req.id);
  if (!r) throw new ApiError(Code.NotFound, `run #${req.id} not found`);
  return toRun(r);
}

export interface RunLogLine {
  /** Seconds since the step started. */
  offsetSeconds: number;
  text: string;
  /** `cmd` = a command the runner echoed before running it. */
  stream: "out" | "err" | "cmd";
  /** Flake check (derivation) this line belongs to, when known. */
  group?: string;
}

/**
 * ASSUMED `GetRunLog` — the log of one step. A running step returns what is
 * there so far (`complete: false`); a real client would stream it
 * (server-streaming RPC) and append.
 */
export async function getRunLog(req: { id: string; step: number }) {
  await mockGate();
  const r = MOCK_RUNS.find((x) => x.id === req.id);
  if (!r) throw new ApiError(Code.NotFound, `run #${req.id} not found`);
  const { lines, complete } = mockRunLog(r, req.step);
  return {
    lines: lines.map<RunLogLine>((l) => ({
      offsetSeconds: l.t,
      text: l.text,
      stream: l.stream,
      group: l.group,
    })),
    complete,
  };
}

// ── mock internals

type Scenario =
  | "slow"
  | "error"
  | "unauth"
  | "empty"
  | "member"
  | "broken"
  | null;

function scenario(): Scenario {
  if (typeof location === "undefined") return null;
  return new URLSearchParams(location.search).get("mock") as Scenario;
}

async function mockGate() {
  const s = scenario();
  await new Promise((r) => setTimeout(r, s === "slow" ? 2500 : 120));
  if (s === "error")
    throw new ApiError(Code.Unavailable, "kajiba-server is unreachable");
  if (s === "unauth")
    throw new ApiError(Code.Unauthenticated, "sign in required");
  if (s === "broken")
    throw new ApiError(
      Code.Unknown,
      "internal: database is locked (SQLITE_BUSY)",
    );
}
