/**
 * Data access for the whole UI — the ONLY module views use to get data.
 *
 * Mirrors `kajiba.v1` with protobuf-es naming (camelCase fields, `{ … }` request
 * objects, numeric enums); every call goes to kajiba-server through the committed
 * connect-web client and the WebAuthn ceremonies.
 */

import { createClient } from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";
import { AdministrationService } from "./gen/kajiba/v1/administration_pb.ts";
import { AuthService, SshKeyUsage } from "./gen/kajiba/v1/auth_pb.ts";
import { PasskeyService } from "./gen/kajiba/v1/passkey_pb.ts";
import { Scope } from "./gen/kajiba/v1/options_pb.ts";
import {
  RunService,
  RunState as RunStateMessage,
  type Attempt as AttemptMessage,
  type Run as RunMessage,
} from "./gen/kajiba/v1/run_pb.ts";
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
import { sortChanges } from "./repo-navigation.ts";
import { RunLog } from "./runlog.ts";

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
  committerName: string;
  committerEmail: string;
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

/**
 * One page of history, or up to `want` commits when set: the server clamps pages at
 * 200 and may end a path-filtered page short (even empty) with a token (ADR 0137), so
 * pages are followed, at most `MAX_PAGES` requests.
 */
export async function listCommits(req: {
  repo: RepoRef;
  revision: string;
  pageSize?: number;
  pageToken?: string;
  /** Only commits touching this file or directory; empty for all. */
  path?: string;
  want?: number;
}) {
  const want = req.want ?? 0;
  const commits: Commit[] = [];
  let pageToken = req.pageToken ?? "";
  for (let page = 0; page < (want ? MAX_PAGES : 1); page++) {
    const res = await repos.listCommits({
      repo: req.repo,
      revision: short(req.revision),
      pageSize: req.pageSize ?? Math.min(want || 50, 200),
      pageToken,
      path: req.path ?? "",
    });
    commits.push(...res.commits.map(toCommit));
    pageToken = res.nextPageToken;
    if (!pageToken || commits.length >= want) break;
  }
  return { commits, nextPageToken: pageToken };
}

const MAX_PAGES = 8;

function toCommit(c: CommitMessage): Commit {
  return {
    id: c.id,
    parentIds: c.parentIds,
    authorName: c.authorName,
    authorEmail: c.authorEmail,
    committerName: c.committerName,
    committerEmail: c.committerEmail,
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
    changes: sortChanges(
      changes.map<Change>((c) => ({
        path: c.path,
        kind: c.kind as number as ChangeKind,
        oldId: c.oldId,
        newId: c.newId,
      })),
    ),
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

/** A declared SSH key: its identity only, read-only (ADR 0143). */
export interface SshKey {
  usage: "auth" | "signing";
  keyType: string;
  /** `SHA256:…` */
  fingerprint: string;
}

export interface Viewer {
  user: string;
  /** Forge administrator (sees Administration). */
  admin: boolean;
  /** Declared SSH keys, authentication then signing. */
  keys: SshKey[];
}

const auth = createClient(AuthService, transport);

/** `AuthService.GetViewer` — who is signed in. Throws `Unauthenticated` otherwise (ADR 0087). */
export async function getViewer(): Promise<Viewer> {
  const { user, admin, keys } = await auth.getViewer({});
  return {
    user,
    admin,
    keys: keys.map((k) => ({
      usage: k.usage === SshKeyUsage.SIGNING ? "signing" : "auth",
      keyType: k.keyType,
      fingerprint: k.fingerprint,
    })),
  };
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
  /** Hostname the runner reported at its last connection; empty before the first. */
  host: string;
  /** Execution backends, e.g. `nix · process`. */
  backend: string;
  /** `<arch>-<os>` then the declared tags. */
  labels: string[];
  state: RunnerState;
  /** Removed from the configuration; its row stays for its attempts. */
  disabled: boolean;
  version: string;
  lastSeenTime?: Timestamp;
  /** The oldest attempt running now. */
  currentRun?: {
    id: string;
    short: string;
    repo: RepoRef;
    ref: string;
    startTime?: Timestamp;
  };
}

const administration = createClient(AdministrationService, transport);

/** `AdministrationService.ListRunners` — administrators only (ADR 0139). */
export async function listRunners() {
  const { runners } = await administration.listRunners({});
  return {
    runners: runners.map<Runner>((r) => {
      const now = r.current[0];
      return {
        name: r.name,
        host: r.hostname,
        backend: r.backends.join(" · "),
        labels: [`${r.arch}-${r.os}`, ...r.tags],
        state: !r.connected
          ? RunnerState.OFFLINE
          : now
            ? RunnerState.BUSY
            : RunnerState.IDLE,
        disabled: r.disabled,
        version: r.version,
        lastSeenTime: r.lastContactAt,
        currentRun: now && {
          id: now.runId,
          short: shortRunId(now.runId),
          repo: now.repo,
          ref: now.ref,
          startTime: now.startedAt,
        },
      };
    }),
  };
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
  /** The last failure or success, whichever is later. */
  lastAttemptTime?: Timestamp;
  /** Unset until a push succeeded: "never pushed". */
  lastSuccessTime?: Timestamp;
  lastError: string;
  /** Consecutive failed attempts (retries with backoff). */
  attempts: number;
  nextAttemptTime?: Timestamp;
}

const epoch = (t?: Timestamp) => Number(t?.seconds ?? 0n);

/** `AdministrationService.ListMirrors` — administrators only (ADR 0139). */
export async function listMirrorStatus() {
  const { mirrors } = await administration.listMirrors({});
  return {
    mirrors: mirrors.map<MirrorStatus>((m) => {
      const failing =
        m.lastError !== "" && epoch(m.lastErrorAt) >= epoch(m.lastSuccessAt);
      return {
        repo: m.repo,
        remoteUrl: m.remoteUrl,
        state: !m.enabled
          ? MirrorState.DISABLED
          : failing
            ? MirrorState.FAILING
            : MirrorState.OK,
        lastAttemptTime: failing ? m.lastErrorAt : m.lastSuccessAt,
        lastSuccessTime: m.lastSuccessAt,
        lastError: m.lastError,
        attempts: Number(m.attempts),
        nextAttemptTime: m.nextAttemptAt,
      };
    }),
  };
}

export interface ServerUser {
  name: string;
  admin: boolean;
  disabled: boolean;
}

export interface ServerInfo {
  version: string;
  startTime?: Timestamp;
  /** 1 at startup, plus one per applied configuration reload (ADR 0141). */
  configGeneration: number;
  databaseBytes: number;
  storageBytes: number;
  declaredTokens: number;
  users: ServerUser[];
}

/** `AdministrationService.GetServerInfo` — administrators only (ADR 0139). */
export async function getServerInfo(): Promise<ServerInfo> {
  const { server } = await administration.getServerInfo({});
  if (!server) throw new ApiError(Code.Unknown, "empty server information");
  return {
    version: server.version,
    startTime: server.startedAt,
    configGeneration: Number(server.configGeneration),
    databaseBytes: Number(server.databaseBytes),
    storageBytes: Number(server.storageBytes),
    declaredTokens: Number(server.declaredTokens),
    users: server.users.map(({ name, admin, disabled }) => ({
      name,
      admin,
      disabled,
    })),
  };
}

// ── Account: passkeys and tokens ────────────────────────────────────────

export interface Passkey {
  id: string;
  /** Label chosen at enrolment; empty when none was given. */
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

const passkeyService = createClient(PasskeyService, transport);

/** `ListPasskeys` — all pages, oldest first. */
export async function listPasskeys() {
  const passkeys: Passkey[] = [];
  let pageToken = "";
  do {
    const page = await passkeyService.listPasskeys({ pageToken });
    passkeys.push(
      ...page.passkeys.map((p) => ({
        id: p.publicId,
        name: p.name,
        createTime: p.createdAt,
        lastUsedTime: p.lastUsedAt,
      })),
    );
    pageToken = page.nextPageToken;
  } while (pageToken);
  return { passkeys };
}

/**
 * `DeletePasskey` — needs a `stepUp()` less than five minutes old; the last passkey is
 * refused (ADR 0143).
 */
export async function deletePasskey(req: { id: string }) {
  await passkeyService.deletePasskey({ publicId: req.id });
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
  SESSION_INVALID: "Your session has ended. Sign in again.",
  STEP_UP_REQUIRED:
    "Confirm with one of your passkeys first; a confirmation lasts five minutes.",
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
    res.status === 503
      ? Code.Unavailable
      : res.status === 401
        ? Code.Unauthenticated
        : Code.PermissionDenied,
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

/**
 * Step-up (ADR 0143): re-authenticates this session with one of the user's passkeys;
 * adding or removing a passkey is then allowed for five minutes.
 */
export async function stepUp(): Promise<void> {
  const started = (await (await ceremony("step-up/start")).json()) as {
    ceremony: string;
    options: { publicKey: PublicKeyCredentialRequestOptionsJSON };
  };
  const publicKey = PublicKeyCredential.parseRequestOptionsFromJSON(
    started.options.publicKey,
  );
  const credential = await prompt(() =>
    navigator.credentials.get({ publicKey }),
  );
  await ceremony("step-up/finish", {
    ceremony: started.ceremony,
    credential: (credential as PublicKeyCredential).toJSON(),
  });
}

/** An additional passkey's ceremony, started after a step-up. */
export interface Registration {
  ceremony: string;
  publicKey: PublicKeyCredentialCreationOptionsJSON;
}

/**
 * Starts registering an additional passkey (ADR 0143). Started before the click that
 * creates it, so that the browser prompt opens from that click.
 */
export async function startPasskeyRegistration(): Promise<Registration> {
  const started = (await (await ceremony("register/start")).json()) as {
    ceremony: string;
    options: { publicKey: PublicKeyCredentialCreationOptionsJSON };
  };
  return { ceremony: started.ceremony, publicKey: started.options.publicKey };
}

/** Creates and stores the additional passkey; the session stays as it is. */
export async function addPasskey(req: {
  registration: Registration;
  name: string;
}): Promise<void> {
  const publicKey = PublicKeyCredential.parseCreationOptionsFromJSON(
    req.registration.publicKey,
  );
  const credential = await prompt(() =>
    navigator.credentials.create({ publicKey }),
  );
  await ceremony("register/finish", {
    ceremony: req.registration.ceremony,
    credential: (credential as PublicKeyCredential).toJSON(),
    name: req.name,
  });
}

// ── RunService ──────────────────────────────────────────────────────────

export enum RunState {
  UNSPECIFIED = 0,
  QUEUED = 1,
  RUNNING = 2,
  SUCCEEDED = 3,
  FAILED = 4,
  CANCELLED = 5,
}

/** One attempt of the run's job: a replayed attempt is a new step. */
export interface RunStep {
  name: string;
  state: RunState;
  durationSeconds: number;
  /** Flake checks of the step, when known (not sent by the server yet). */
  checks?: string[];
}

export interface RunFailure {
  /** The failing check when the runner knows it; often empty. */
  check: string;
  /** E.g. `exit status 1`, `lease-expired`. */
  reason: string;
  lastLines: string[];
}

export interface Run {
  id: string;
  /** The public id shortened for display. */
  short: string;
  repo: RepoRef;
  /** Full ref the run was triggered for; empty for a manual or worktree run. */
  ref: string;
  commitId: string;
  state: RunState;
  createTime?: Timestamp;
  /** Elapsed so far while running. */
  durationSeconds: number;
  /** Declared name of the runner of the latest attempt; empty before one. */
  runner: string;
  runnerHostname: string;
  trigger: "push" | "manual" | "worktree";
  /** The user who pushed or triggered; empty for an import. */
  triggeredBy: string;
  /** Why no runner takes the job (ADR 0109); empty otherwise. */
  pendingReason: string;
  failure?: RunFailure;
  /** One step per known attempt; a run not assigned yet has one queued step. */
  steps: RunStep[];
}

const runs = createClient(RunService, transport);

/** Run ids are 26 characters, the leading ones being the time: show the random tail. */
const shortRunId = (id: string) => id.slice(-8).toLowerCase();

const seconds = (t?: { seconds: bigint }) => (t ? Number(t.seconds) : 0);

/** Elapsed seconds from start to finish, or to now while unfinished. */
function elapsed(
  started?: { seconds: bigint },
  finished?: { seconds: bigint },
): number {
  if (!started) return 0;
  const end = finished ? seconds(finished) : Date.now() / 1000;
  return Math.max(0, Math.round(end - seconds(started)));
}

// Lost = the runner vanished: a failure of the attempt.
const ATTEMPT_STATE = [
  RunState.UNSPECIFIED,
  RunState.RUNNING,
  RunState.SUCCEEDED,
  RunState.FAILED,
  RunState.CANCELLED,
  RunState.FAILED,
];
const TRIGGER = [undefined, "push", "manual", "worktree"] as const;

function toStep(a: AttemptMessage): RunStep {
  return {
    name: `attempt ${a.number}`,
    state: ATTEMPT_STATE[a.state] ?? RunState.UNSPECIFIED,
    durationSeconds: elapsed(a.startedAt, a.finishedAt),
  };
}

function toRun(m: RunMessage, repo: RepoRef): Run {
  const attempt = m.job?.currentAttempt;
  return {
    id: m.publicId,
    short: shortRunId(m.publicId),
    repo,
    ref: m.ref,
    commitId: m.sha,
    state: m.state as number as RunState,
    createTime: m.createdAt,
    durationSeconds: elapsed(m.startedAt, m.finishedAt),
    runner: attempt?.runner ?? "",
    runnerHostname: attempt?.runnerHostname ?? "",
    trigger: TRIGGER[m.trigger] ?? "push",
    triggeredBy: m.triggeredBy,
    pendingReason: m.job?.pendingReason ?? "",
    failure: m.failure && {
      check: m.failure.check,
      reason: m.failure.reason,
      lastLines: m.failure.lastLogLines,
    },
    steps: attempt
      ? [toStep(attempt)]
      : [
          {
            name: m.job?.name || "check",
            state:
              m.state === RunStateMessage.CANCELLED
                ? RunState.CANCELLED
                : RunState.QUEUED,
            durationSeconds: 0,
          },
        ],
  };
}

/** A repository's runs, newest first; `ref` is an exact full ref (ADR 0138). */
export async function listRuns(req: {
  repo: RepoRef;
  ref?: string;
  pageSize?: number;
  pageToken?: string;
}) {
  const res = await runs.listRuns({
    repo: req.repo,
    ref: req.ref ?? "",
    pageSize: req.pageSize ?? 50,
    pageToken: req.pageToken ?? "",
  });
  return {
    runs: res.runs.map((r) => toRun(r, req.repo)),
    nextPageToken: res.nextPageToken,
  };
}

/** One run by public id; NotFound if unknown or its repository is unreadable. */
export async function getRun(req: { id: string }): Promise<Run> {
  const { run } = await runs.getRun({ run: req.id });
  if (!run) throw new ApiError(Code.Unknown, "empty GetRun answer");
  const { repo } = await repos.getRepo({
    repo: { case: "publicId", value: run.repoPublicId },
  });
  return toRun(run, repo ? `${repo.owner}/${repo.name}` : run.repoPublicId);
}

export interface RunLogLine {
  /** Seconds since the step started (the server does not time lines: 0). */
  offsetSeconds: number;
  text: string;
  /** `err` = an `error:` line; `cmd` = a command the runner echoed. */
  stream: "out" | "err" | "cmd";
  /** Flake check (derivation) this line belongs to, when known. */
  group?: string;
}

/** Gives up a stream after this many consecutive failures; any message resets the count. */
const WATCH_RETRIES = 5;

const pause = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((done) => {
    const t = setTimeout(done, ms);
    signal?.addEventListener("abort", () => (clearTimeout(t), done()), {
      once: true,
    });
  });

/** An empty log to feed with `watchRun`. */
export const newRunLog = () => new RunLog<RunStep>();

/**
 * Reads a run's log into `log` from `log.offset` (ADR 0142): the stored bytes,
 * then, with `follow`, the live ones until the run is terminal, whose final
 * state it returns. Keepalives are ignored; a broken stream is resumed from the
 * received offset; `signal` aborts it (a tab holds one of 16 watches, ADR 0120).
 * `onChange` is called at most every 100 ms and once at the end.
 */
export async function watchRun(req: {
  id: string;
  repo: RepoRef;
  log: RunLog<RunStep>;
  follow: boolean;
  signal?: AbortSignal;
  onChange?: () => void;
}): Promise<Run | undefined> {
  const { log, signal } = req;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const changed = () => {
    timer ??= setTimeout(() => {
      timer = undefined;
      req.onChange?.();
    }, 100);
  };
  try {
    for (let failures = 0; ; ) {
      try {
        for await (const m of runs.watchRun(
          { run: req.id, offset: BigInt(log.offset), follow: req.follow },
          { signal },
        )) {
          failures = 0;
          const e = m.event;
          if (e.case === "section" && e.value.attempt)
            log.section(e.value.attempt.publicId, toStep(e.value.attempt));
          else if (e.case === "log")
            log.bytes(Number(e.value.offset), e.value.data);
          else if (e.case === "finished") return toRun(e.value, req.repo);
          else continue;
          changed();
        }
        // A following stream ending without the final state (server stopping): resume.
        if (!req.follow || signal?.aborted) return undefined;
        throw new ApiError(Code.Unavailable, "log stream ended");
      } catch (err) {
        const code = (err as { code?: number }).code;
        if (
          signal?.aborted ||
          code === Code.NotFound ||
          code === Code.PermissionDenied ||
          code === Code.Unauthenticated ||
          code === Code.ResourceExhausted ||
          ++failures > WATCH_RETRIES
        ) {
          if (signal?.aborted) return undefined;
          throw err;
        }
        await pause(Math.min(500 * 2 ** failures, 8000), signal);
        if (signal?.aborted) return undefined;
      }
    }
  } finally {
    clearTimeout(timer);
    req.onChange?.();
  }
}
