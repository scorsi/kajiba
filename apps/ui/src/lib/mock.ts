/**
 * Static mock data for the design prototype. Only `api.ts` imports this file;
 * the integration session deletes it together with the mock `api.ts`.
 *
 * Simplifications (fine for design, not for integration):
 * - every ref of a repository serves the same file tree;
 * - a ref is just an index into the repository's commit list (history = slice from there);
 * - commit ids and object ids are deterministic fake hashes.
 */

// ── tiny deterministic helpers ─────────────────────────────────────────

function hash32(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++)
    h = Math.imul(h ^ s.charCodeAt(i), 0x01000193);
  return h >>> 0;
}

/** Fake 40-hex object id derived from a seed string. */
export function fakeId(seed: string): string {
  let out = "";
  for (let i = 0; out.length < 40; i++)
    out += hash32(seed + "#" + i)
      .toString(16)
      .padStart(8, "0");
  return out.slice(0, 40);
}

export const ZERO_ID = "0".repeat(40);

// ── file trees ─────────────────────────────────────────────────────────

export type MockNode =
  | { kind: "blob"; content: string | Uint8Array | (() => Uint8Array) }
  | { kind: "tree"; children: Record<string, MockNode> }
  | { kind: "symlink"; target: string }
  | { kind: "submodule"; url: string };

const b = (content: string | Uint8Array | (() => Uint8Array)): MockNode => ({
  kind: "blob",
  content,
});
const t = (children: Record<string, MockNode>): MockNode => ({
  kind: "tree",
  children,
});

const PNG_LOGO = Uint8Array.from(
  atob(
    "iVBORw0KGgoAAAANSUhEUgAAABgAAAAYCAYAAADgdz34AAAARElEQVR42mNgGAWkgC0u0v+pgQevBf8PNMAxVS1ANhgdD34L8BmOy5IRZsHQj2S65AOq5eQoFYH/1MCjFoxkC0YBNgAAzIauR4QFouIAAAAASUVORK5CYII=",
  ),
  (c) => c.charCodeAt(0),
);

/** ~1.6 MB of NDJSON, generated on first read. */
function largeNdjson(): Uint8Array {
  const lines: string[] = [];
  for (let i = 0; i < 12000; i++) {
    lines.push(
      JSON.stringify({
        seq: i,
        ref: "refs/heads/main",
        old: fakeId("o" + i),
        new: fakeId("n" + i),
        pusher: i % 7 === 0 ? "mika" : "scorsi",
        at: 1767225600 + i * 613,
      }),
    );
  }
  return new TextEncoder().encode(lines.join("\n") + "\n");
}

function randomBytes(n: number, seed: string): Uint8Array {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = hash32(seed + i) & 0xff;
  return out;
}

const KAJIBA_README = `# kajiba

**kajiba** (鍛冶場, *the forge*) is a small, declarative, self-hosted git forge.
One owner, a few users, a few dozen repositories — configured entirely from Nix.

> Repositories, permissions, mirrors, runners and SSH keys are declared in the
> server configuration. The web interface only *shows* them.

## Features

- Git over HTTPS (read-only) and SSH (read/write), every pushed commit SSH-signed
- Passkey-only sign-in (WebAuthn), personal access tokens for the CLI
- CI runners speaking a small gRPC protocol, \`nix flake check\` as the pipeline
- Push mirroring to GitHub with retries and alerting

## Quick start

Add the flake input and enable the module:

\`\`\`nix
{
  inputs.kajiba.url = "git+https://git.example.org/scorsi/kajiba";

  outputs = { kajiba, ... }: {
    nixosConfigurations.builder = {
      modules = [
        kajiba.nixosModules.default
        { services.kajiba.enable = true; }
      ];
    };
  };
}
\`\`\`

Then clone a repository:

\`\`\`sh
git clone ssh://kajiba-git@git.example.org:2222/scorsi/kajiba.git
\`\`\`

## Components

| Component        | Language | Role                                   |
| ---------------- | -------- | -------------------------------------- |
| \`kajiba-server\`  | Rust     | git transport, gRPC API, web bundle    |
| \`kajiba-runner\`  | Rust     | executes CI jobs (nix → nspawn → microvm) |
| \`kajiba-cli\`     | Go       | create tokens, follow CI runs          |
| \`kajiba-frontend\`| Svelte   | this web interface                     |

## Status

- [x] Repository hosting (SSH + HTTPS)
- [x] Passkeys and tokens
- [ ] CI runs in the UI
- [ ] Mirror status

See [docs/adr](docs/adr) for the architecture decision records.

## Licence

Apache-2.0.
`;

const RUST_MAIN = `//! kajiba-server entry point.

use std::net::SocketAddr;

use anyhow::Context;
use kajiba_server::{config::Config, telemetry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    let config = Config::load_from_env().context("loading configuration")?;

    let addr: SocketAddr = config.listen.parse()?;
    tracing::info!(%addr, repos = config.repositories.len(), "starting kajiba-server");

    kajiba_server::serve(config, addr).await
}
`;

const kajibaTree = t({
  ".github": t({ CODEOWNERS: b("* @scorsi\n") }),
  crates: t({
    "kajiba-server": t({
      "Cargo.toml": b(
        '[package]\nname = "kajiba-server"\nversion = "0.4.0"\nedition = "2024"\nlicense = "Apache-2.0"\n\n[dependencies]\nanyhow = "1"\ntokio = { version = "1", features = ["full"] }\ntracing = "0.1"\n',
      ),
      src: t({
        "main.rs": b(RUST_MAIN),
        "lib.rs": b(
          "pub mod config;\npub mod git;\npub mod rpc;\npub mod telemetry;\n",
        ),
        git: t({
          "mod.rs": b("pub mod refs;\npub mod tree;\n"),
          "refs.rs": b("// Ref resolution.\n"),
          "tree.rs": b("// Tree walking.\n"),
          transport: t({
            "http.rs": b("// Smart HTTP, read-only.\n"),
            "ssh.rs": b("// SSH via AuthorizedKeysCommand.\n"),
          }),
        }),
        rpc: t({
          "repo_service.rs": b("// kajiba.v1.RepoService implementation.\n"),
        }),
      }),
    }),
    "kajiba-runner": t({
      "Cargo.toml": b(
        '[package]\nname = "kajiba-runner"\nversion = "0.4.0"\nedition = "2024"\n',
      ),
      src: t({ "main.rs": b("fn main() {}\n") }),
    }),
  }),
  docs: t({
    adr: t({
      "0001-git-library.md": b(
        "# ADR 0001 — Git library for reads\n\nAccepted.\n",
      ),
      "0002-ssh-transport.md": b("# ADR 0002 — System sshd\n\nAccepted.\n"),
    }),
    assets: t({ "logo.png": b(PNG_LOGO) }),
  }),
  frontend: {
    kind: "submodule",
    url: "https://git.example.org/scorsi/kajiba-frontend.git",
  },
  proto: t({
    kajiba: t({
      v1: t({
        "repo.proto": b(
          'syntax = "proto3";\n\npackage kajiba.v1;\n\nservice RepoService {\n  rpc ListRepos(ListReposRequest) returns (ListReposResponse);\n  rpc GetTree(GetTreeRequest) returns (GetTreeResponse);\n}\n',
        ),
      }),
    }),
  }),
  testdata: t({
    "push-log.ndjson": b(largeNdjson),
    "pack.bin": b(() => randomBytes(4096, "pack")),
  }),
  ".gitignore": b("/target\n/result\n"),
  "Cargo.lock": b(
    "# This file is automatically @generated by Cargo.\nversion = 4\n",
  ),
  "Cargo.toml": b('[workspace]\nmembers = ["crates/*"]\nresolver = "3"\n'),
  LICENSE: b("Apache License\nVersion 2.0, January 2004\n"),
  "README.md": b(KAJIBA_README),
  "flake.lock": b('{ "nodes": {}, "version": 7 }\n'),
  "flake.nix": b(
    '{\n  description = "kajiba — a small declarative git forge";\n\n  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";\n\n  outputs = { self, nixpkgs }: { };\n}\n',
  ),
  "CONTRIBUTING.md": { kind: "symlink", target: "docs/CONTRIBUTING.md" },
});

const infraTree = t({
  hosts: t({
    laptop: t({
      "default.nix": b('{ ... }: {\n  networking.hostName = "laptop";\n}\n'),
    }),
    builder: t({
      "default.nix": b("{ ... }: {\n  services.kajiba.enable = true;\n}\n"),
    }),
  }),
  modules: t({ "kajiba.nix": b("{ config, lib, ... }: { }\n") }),
  "flake.nix": b(
    '{\n  description = "infra — my homelab";\n  outputs = { ... }: { };\n}\n',
  ),
  "README.md": b(
    "# infra\n\n設計図 — the blueprint of my homelab: a Mac Mini host and its NixOS guests.\n\n```sh\nnix flake check\n```\n",
  ),
});

const hikoukiTree = t({
  src: t({ "main.rs": b('fn main() {\n    println!("hikouki");\n}\n') }),
  "Cargo.toml": b(
    '[package]\nname = "hikouki"\nversion = "0.1.0"\nedition = "2024"\n',
  ),
  // No README on purpose: exercises the "no README" state.
});

const notesTree = t({
  "2026": t({
    "10-japanese.md": b(
      "# 日本語メモ\n\n- 鍛冶場 (かじば) — forge\n- 設計図 (せっけいず) — blueprint\n",
    ),
  }),
  "README.md": b("# notes\n\nMika’s scratch notes.\n"),
});

const dotfilesTree = t({
  zsh: t({ ".zshrc": b("export EDITOR=hx\n") }),
  "README.md": b(
    "# dotfiles\n\nSuperseded by the NixOS configuration in `infra`. Kept for reference.\n",
  ),
});

// ── commits ────────────────────────────────────────────────────────────

export interface MockCommit {
  id: string;
  parentIds: string[];
  authorName: string;
  authorEmail: string;
  time: number; // unix seconds
  message: string;
}

const SUBJECTS = [
  "feat(server): serve tree listings over RepoService",
  "fix(ssh): reject pushes to archived repositories",
  "refactor(git): split ref resolution from tree walking",
  "feat(runner): advertise labels on enrolment",
  "docs(adr): record the mirror retry policy",
  "chore(deps): bump tokio to 1.48",
  "test(e2e): cover passkey enrolment link expiry",
  "feat(auth): revoke personal access tokens",
  "fix(http): stream large packs without buffering",
  "perf(git): cache resolved default branch per request",
  "feat(mirror): exponential backoff with alerting",
  "chore(nix): expose devShell with buf and pnpm",
  "fix(rpc): return NOT_FOUND for unknown paths",
  "build: embed the frontend bundle in the server binary",
  "feat(frontend): repository home page",
];

const BODY =
  "The previous behaviour accepted the request and failed late in the\ntransport. Fail early with a clear status instead.\n\nRefs: J7";

function makeHistory(
  repo: string,
  count: number,
  newest: number,
  authors: [string, string][],
) {
  const commits: MockCommit[] = [];
  let time = newest;
  for (let i = 0; i < count; i++) {
    const [authorName, authorEmail] =
      authors[hash32(repo + i) % authors.length];
    const subject = SUBJECTS[hash32(repo + "s" + i) % SUBJECTS.length];
    const isMerge = i > 0 && i % 23 === 0;
    commits.push({
      id: fakeId(repo + "@" + i),
      parentIds: [],
      authorName,
      authorEmail,
      time,
      message: isMerge
        ? `Merge branch 'feat/${subject.split(":")[0].replace(/\W+/g, "-")}'\n`
        : i % 4 === 0
          ? `${subject}\n\n${BODY}\n`
          : `${subject}\n`,
    });
    time -= 3600 + (hash32(repo + "t" + i) % (3600 * 30));
  }
  for (let i = 0; i < commits.length - 1; i++) {
    commits[i].parentIds = [commits[i + 1].id];
    if (commits[i].message.startsWith("Merge"))
      commits[i].parentIds.push(fakeId(repo + "side" + i));
  }
  return commits;
}

const SCORSI: [string, string] = ["Scorsi", "scorsi@kajiba.invalid"];
const MIKA: [string, string] = ["Mika Arai", "mika@kajiba.invalid"];
const NOW = Math.floor(Date.now() / 1000);

// ── repositories ───────────────────────────────────────────────────────

export interface MockRepo {
  publicId: string;
  owner: string;
  name: string;
  description: string;
  defaultBranch: string;
  ci: "off" | "manual" | "push";
  previousNames: string[];
  permissions: { user: string; role: "read" | "write" | "admin" }[];
  mirrors: { remoteUrl: string; credential: string; enabled: boolean }[];
  archived: boolean;
  tree: MockNode;
  commits: MockCommit[];
  /** ref full name → index into `commits` */
  refs: Record<string, number>;
}

export const MOCK_REPOS: MockRepo[] = [
  {
    publicId: "r_7k2m9q",
    owner: "scorsi",
    name: "kajiba",
    description: "A small, declarative, self-hosted git forge.",
    defaultBranch: "refs/heads/main",
    ci: "push",
    previousNames: ["forge"],
    permissions: [
      { user: "scorsi", role: "admin" },
      { user: "mika", role: "write" },
      { user: "ren", role: "read" },
    ],
    mirrors: [
      {
        remoteUrl: "git@github.com:scorsi/kajiba.git",
        credential: "github-deploy-key",
        enabled: true,
      },
    ],
    archived: false,
    tree: kajibaTree,
    commits: makeHistory("kajiba", 420, NOW - 3 * 3600, [
      SCORSI,
      SCORSI,
      SCORSI,
      MIKA,
    ]),
    refs: {
      "refs/heads/main": 0,
      "refs/heads/feat/runner-labels": 2,
      "refs/heads/fix/mirror-backoff": 5,
      "refs/tags/v0.4.0": 18,
      "refs/tags/v0.3.0": 77,
      "refs/tags/v0.2.0": 160,
      "refs/tags/v0.1.0": 300,
    },
  },
  {
    publicId: "r_3fh81c",
    owner: "scorsi",
    name: "infra",
    description: "My homelab: nix-darwin host and NixOS guests.",
    defaultBranch: "refs/heads/main",
    ci: "push",
    previousNames: [],
    permissions: [{ user: "scorsi", role: "admin" }],
    mirrors: [
      {
        remoteUrl: "git@github.com:scorsi/infra.git",
        credential: "github-deploy-key",
        enabled: true,
      },
    ],
    archived: false,
    tree: infraTree,
    commits: makeHistory("infra", 64, NOW - 26 * 3600, [SCORSI]),
    refs: { "refs/heads/main": 0 },
  },
  {
    publicId: "r_9xq2la",
    owner: "scorsi",
    name: "hikouki",
    description: "Starship-like toolkit for Claude Code.",
    defaultBranch: "refs/heads/main",
    ci: "off",
    previousNames: [],
    permissions: [{ user: "scorsi", role: "admin" }],
    mirrors: [
      {
        remoteUrl: "git@github.com:scorsi/hikouki.git",
        credential: "github-deploy-key",
        enabled: true,
      },
    ],
    archived: false,
    tree: hikoukiTree,
    commits: makeHistory("hikouki", 9, NOW - 9 * 86400, [SCORSI]),
    refs: { "refs/heads/main": 0, "refs/tags/v0.1.0": 2 },
  },
  {
    publicId: "r_44pd0z",
    owner: "scorsi",
    name: "scratch",
    description: "",
    defaultBranch: "refs/heads/main",
    ci: "off",
    previousNames: [],
    permissions: [{ user: "scorsi", role: "admin" }],
    mirrors: [],
    archived: false,
    tree: t({}),
    commits: [], // empty repository: no refs at all
    refs: {},
  },
  {
    publicId: "r_0c1dot",
    owner: "scorsi",
    name: "dotfiles",
    description: "Old dotfiles, superseded by infra.",
    defaultBranch: "refs/heads/master",
    ci: "off",
    previousNames: [],
    permissions: [{ user: "scorsi", role: "admin" }],
    mirrors: [
      {
        remoteUrl: "git@github.com:scorsi/dotfiles.git",
        credential: "github-deploy-key",
        enabled: false,
      },
    ],
    archived: true,
    tree: dotfilesTree,
    commits: makeHistory("dotfiles", 40, NOW - 400 * 86400, [SCORSI]),
    refs: { "refs/heads/master": 0 },
  },
  {
    publicId: "r_m1n0te",
    owner: "mika",
    name: "notes",
    description: "Scratch notes.",
    defaultBranch: "refs/heads/main",
    ci: "off",
    previousNames: [],
    permissions: [
      { user: "mika", role: "admin" },
      { user: "scorsi", role: "read" },
    ],
    mirrors: [],
    archived: false,
    tree: notesTree,
    commits: makeHistory("notes", 12, NOW - 4 * 86400, [MIKA]),
    refs: { "refs/heads/main": 0 },
  },
];

/** Signed-in user of the mock session. */

// ── runners & stats (ASSUMED services, see api.ts) ──────────────────────

export const MOCK_RUNNERS = [
  {
    name: "builder-1",
    host: "builder",
    backend: "nix",
    labels: ["nix", "x86_64-linux"],
    state: "busy" as const,
    lastSeen: NOW - 4,
    currentRun: {
      id: "1287",
      repo: "scorsi/kajiba",
      ref: "refs/heads/main",
      startedAt: NOW - 140,
    },
  },
  {
    name: "builder-2",
    host: "builder",
    backend: "nspawn",
    labels: ["nix", "x86_64-linux", "nspawn"],
    state: "idle" as const,
    lastSeen: NOW - 9,
  },
  {
    name: "laptop-1",
    host: "laptop",
    backend: "nix",
    labels: ["nix", "aarch64-darwin"],
    state: "idle" as const,
    lastSeen: NOW - 21,
  },
  {
    name: "laptop-vm",
    host: "laptop",
    backend: "microvm",
    labels: ["nix", "aarch64-linux", "microvm"],
    state: "offline" as const,
    lastSeen: NOW - 2 * 86400 - 3600,
  },
];

/** Repository storage on disk (GetServerInfo). */
export const MOCK_DISK_BYTES = 1_842_000_000;

/** Push-mirror outcome per (repo, remote). */
export const MOCK_MIRROR_STATUS: Record<
  string,
  {
    state: "ok" | "failing" | "disabled";
    lastAttempt?: number;
    lastSuccess?: number;
    error?: string;
    attempts?: number;
    nextAttempt?: number;
  }
> = {
  "scorsi/kajiba git@github.com:scorsi/kajiba.git": {
    state: "ok",
    lastAttempt: NOW - 3 * 3600,
    lastSuccess: NOW - 3 * 3600,
  },
  "scorsi/infra git@github.com:scorsi/infra.git": {
    state: "ok",
    lastAttempt: NOW - 26 * 3600,
    lastSuccess: NOW - 26 * 3600,
  },
  "scorsi/hikouki git@github.com:scorsi/hikouki.git": {
    state: "failing",
    lastAttempt: NOW - 600,
    lastSuccess: NOW - 9 * 86400,
    error: "remote: Permission to scorsi/hikouki.git denied to deploy key.",
    attempts: 7,
    nextAttempt: NOW + 1800,
  },
  "scorsi/dotfiles git@github.com:scorsi/dotfiles.git": {
    state: "disabled",
    lastSuccess: NOW - 400 * 86400,
  },
};

export const MOCK_SERVER = {
  version: "0.4.0",
  commit: fakeId("server-build"),
  startedAt: NOW - 6 * 86400 - 5 * 3600,
  configGeneration: "infra@" + fakeId("gen").slice(0, 7),
  databaseBytes: 18_400_000,
  declaredTokens: 2,
  users: ["scorsi", "mika", "ren"],
};

// ── account (ASSUMED auth service) — mutable, in memory ─────────────────

export const MOCK_PASSKEYS = [
  {
    id: "pk_1",
    name: "YubiKey 5C NFC",
    created: NOW - 40 * 86400,
    lastUsed: NOW - 3 * 3600,
  },
  {
    id: "pk_2",
    name: "YubiKey 5C (backup)",
    created: NOW - 40 * 86400,
    lastUsed: NOW - 31 * 86400,
  },
];

export const MOCK_TOKENS = [
  {
    id: "tok_ci",
    name: "kajiba-cli on laptop",
    scopes: ["repo:read", "ci:trigger"],
    created: NOW - 12 * 86400,
    expires: NOW + 78 * 86400,
    lastUsed: NOW - 2 * 3600,
    declared: false,
  },
  {
    id: "tok_old",
    name: "laptop scripts",
    scopes: ["repo:read"],
    created: NOW - 85 * 86400,
    expires: NOW + 5 * 86400,
    lastUsed: NOW - 20 * 86400,
    declared: false,
  },
  {
    id: "tok_backup",
    name: "backup-pull",
    scopes: ["repo:read"],
    created: NOW - 200 * 86400,
    expires: undefined,
    lastUsed: NOW - 9 * 3600,
    declared: true,
  },
  {
    id: "tok_infra",
    name: "infra-deploy",
    scopes: ["repo:read"],
    created: NOW - 200 * 86400,
    expires: NOW + 165 * 86400,
    lastUsed: NOW - 26 * 3600,
    declared: true,
  },
];

// ── CI runs (ASSUMED run service) ───────────────────────────────────────

export interface MockRun {
  id: string;
  repo: string;
  ref: string;
  commitId: string;
  state: "queued" | "running" | "succeeded" | "failed" | "cancelled";
  created: number;
  duration: number; // seconds (elapsed so far when running)
  runner: string;
  trigger: "push" | "manual";
  steps: { name: string; state: MockRun["state"]; duration: number }[];
}

const STEP_NAMES = ["Fetch", "Evaluate flake", "nix flake check", "Report"];

function makeRuns(repo: MockRepo, firstId: number, count: number): MockRun[] {
  const runners = ["builder-1", "builder-2", "laptop-1"];
  return repo.commits.slice(0, count).map((c, i) => {
    const h = hash32(c.id);
    const state: MockRun["state"] =
      i === 0 && repo.name === "kajiba"
        ? "running"
        : h % 7 === 0
          ? "failed"
          : h % 29 === 0
            ? "cancelled"
            : "succeeded";
    const total = 120 + (h % 280);
    const failAt = state === "failed" ? 1 + (h % 3) : -1;
    const steps = STEP_NAMES.map((name, s) => {
      const d = Math.round(total * [0.08, 0.17, 0.7, 0.05][s]);
      let st: MockRun["state"] = "succeeded";
      if (state === "running")
        st = s < 2 ? "succeeded" : s === 2 ? "running" : "queued";
      if (state === "failed")
        st = s < failAt ? "succeeded" : s === failAt ? "failed" : "cancelled";
      if (state === "cancelled") st = s < 2 ? "succeeded" : "cancelled";
      return {
        name,
        state: st,
        duration: st === "queued" || st === "cancelled" ? 0 : d,
      };
    });
    return {
      id: String(firstId - i),
      repo: `${repo.owner}/${repo.name}`,
      ref: repo.defaultBranch,
      commitId: c.id,
      state,
      created: c.time + 20,
      duration: state === "running" ? 140 : total,
      runner: state === "running" ? "builder-1" : runners[h % runners.length],
      trigger: h % 11 === 0 ? "manual" : "push",
      steps,
    };
  });
}

export const MOCK_RUNS: MockRun[] = [
  ...makeRuns(MOCK_REPOS[0], 1287, 60),
  ...makeRuns(MOCK_REPOS[1], 912, 30),
];

// ── CI logs (ASSUMED GetRunLog) ─────────────────────────────────────────

export interface MockLogLine {
  /** Seconds since the step started. */
  t: number;
  text: string;
  stream: "out" | "err" | "cmd";
  /** The flake check (derivation) the line belongs to, if any. */
  group?: string;
}

const TESTS = [
  "git::refs::resolves_default_branch",
  "git::refs::rejects_dangling_symref",
  "git::tree::lists_root",
  "git::tree::follows_symlinks",
  "git::blob::detects_binary",
  "git::blob::caps_size",
  "http::pack::serves_info_refs",
  "http::pack::streams_large_packs",
  "http::pack::rejects_push",
  "ssh::auth::accepts_signed_push",
  "ssh::auth::rejects_unsigned_push",
  "ssh::auth::rejects_archived_repo",
  "rpc::repo::list_repos_paginates",
  "rpc::repo::get_tree_not_found",
  "rpc::repo::get_blob_range",
  "rpc::commits::path_filter",
  "config::validate::reserved_owner_dash",
  "config::validate::duplicate_repo",
  "config::validate::unknown_runner_label",
  "mirror::backoff::exponential",
  "mirror::backoff::caps_at_one_hour",
  "mirror::push::reports_failure",
  "ci::runner::assigns_by_label",
  "ci::runner::requeues_on_disconnect",
  "ci::log::streams_in_order",
  "auth::passkey::enrolment_expires",
  "auth::token::scopes_enforced",
  "auth::token::declared_tokens_readonly",
  "frontend::embed::serves_index",
  "frontend::embed::spa_fallback",
];
const CRATES = [
  "proc-macro2 v1.0.95",
  "serde v1.0.219",
  "tokio v1.48.0",
  "prost v0.14.1",
  "gix v0.73.0",
  "axum v0.8.4",
  "rusqlite v0.37.0",
  "kajiba-proto v0.4.1 (/build/source/proto)",
];
export const CHECKS = ["fmt", "clippy", "frontend", "tests", "nixos-smoke"];

/**
 * Deterministic log of one step of a run. A running step is cut where the
 * run is now (`complete: false`); a step that never ran has no lines.
 */
export function mockRunLog(
  run: MockRun,
  step: number,
): { lines: MockLogLine[]; complete: boolean } {
  const st = run.steps[step];
  if (
    !st ||
    st.state === "queued" ||
    (st.state === "cancelled" && st.duration === 0)
  )
    return { lines: [], complete: st?.state !== "queued" };
  const repo = MOCK_REPOS.find((r) => `${r.owner}/${r.name}` === run.repo)!;
  const commit = repo.commits.find((c) => c.id === run.commitId);
  const subject = commit?.message.split("\n")[0] ?? "";
  const short = run.commitId.slice(0, 7);
  const h = hash32(run.id);
  const failed = st.state === "failed";
  const raw: Omit<MockLogLine, "t">[] = [];
  const out = (text: string, group?: string) =>
    raw.push({ text, stream: "out", group });
  const err = (text: string, group?: string) =>
    raw.push({ text, stream: "err", group });
  const cmd = (text: string, group?: string) =>
    raw.push({ text, stream: "cmd", group });
  const drv = (name: string) =>
    `/nix/store/${fakeId(name + run.id).slice(0, 32)}-${name}.drv`;

  if (step === 0) {
    cmd(`git fetch --no-tags --depth=1 kajiba:${run.repo} ${run.ref}`);
    out(`From kajiba:${run.repo}`);
    out(
      ` * branch            ${run.ref.replace("refs/heads/", "")} -> FETCH_HEAD`,
    );
    cmd(`git checkout --detach ${short}`);
    out(`HEAD is now at ${short} ${subject}`);
    out(
      `Good "git" signature for ${commit?.authorName.toLowerCase().split(" ")[0] ?? "scorsi"} with ED25519 key SHA256:${fakeId("key").slice(0, 24)}`,
    );
    out("checkout done in 0.8s");
  } else if (step === 1) {
    cmd("nix eval .#checks.x86_64-linux --apply builtins.attrNames --json");
    if (failed) {
      err("error: attribute 'kajiba-frontend' missing");
      err("       at /nix/store/9x1q…-source/flake.nix:48:21:");
      err("           47|       checks.${system}.frontend =");
      err("           48|         self.packages.${system}.kajiba-frontend;");
      err("             |                     ^");
      err("       Did you mean 'kajiba-server'?");
      err("       (use '--show-trace' to show detailed location information)");
    } else {
      out(JSON.stringify(CHECKS));
      cmd(
        "nix build --dry-run .#checks.x86_64-linux.{" + CHECKS.join(",") + "}",
      );
      out(`these ${CHECKS.length} derivations will be built:`);
      for (const c of CHECKS) out(`  ${drv("kajiba-" + c + "-0.4.1")}`);
      out(
        `these ${180 + (h % 60)} paths will be fetched (412.6 MiB download, 1.9 GiB unpacked):`,
      );
      out("  from https://cache.infra (substituter)");
    }
  } else if (step === 2) {
    cmd("nix flake check --print-build-logs --keep-going");
    const failTest =
      failed && /frontend|embed/.test(subject)
        ? "frontend::embed::spa_fallback"
        : failed
          ? TESTS[h % TESTS.length]
          : "";
    // fmt
    let g = "fmt";
    out(`building '${drv("kajiba-fmt-0.4.1")}'...`, g);
    cmd("treefmt --fail-on-change --no-cache", g);
    out(`traversed ${300 + (h % 90)} files`, g);
    out("emitted 0 files for processing", g);
    out("formatted 0 files in 412ms", g);
    // clippy
    g = "clippy";
    out(`building '${drv("kajiba-clippy-0.4.1")}'...`, g);
    for (const c of CRATES) out(`   Compiling ${c}`, g);
    out("    Checking kajiba-server v0.4.1 (/build/source/server)", g);
    out("    Finished `release` profile [optimized] target(s) in 48.21s", g);
    // frontend
    g = "frontend";
    out(`building '${drv("kajiba-frontend-check-0.4.1")}'...`, g);
    cmd("pnpm check", g);
    out("svelte-check found 0 errors and 0 warnings", g);
    cmd("pnpm build", g);
    out("vite v8.3.0 building for production...", g);
    out("✓ 412 modules transformed.", g);
    out(
      "build/_app/immutable/entry/start.C1u7.js       0.08 kB │ gzip:  0.09 kB",
      g,
    );
    out(
      "build/_app/immutable/chunks/index.B2xk.js     41.37 kB │ gzip: 15.02 kB",
      g,
    );
    out(
      "build/_app/immutable/nodes/2.D9qa.js          28.10 kB │ gzip:  9.84 kB",
      g,
    );
    out("✓ built in 3.41s", g);
    // tests
    g = "tests";
    out(`building '${drv("kajiba-tests-0.4.1")}'...`, g);
    out(
      "    Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 02s",
      g,
    );
    out(
      "     Running unittests src/main.rs (target/debug/deps/kajiba_server-3f2a)",
      g,
    );
    out("", g);
    out(`running ${TESTS.length} tests`, g);
    for (const name of TESTS)
      if (name === failTest) err(`test ${name} ... FAILED`, g);
      else out(`test ${name} ... ok`, g);
    out("", g);
    if (failTest) {
      out("failures:", g);
      out("", g);
      out(`---- ${failTest} stdout ----`, g);
      const mod = failTest.split("::").slice(0, 2).join("/");
      err(
        `thread '${failTest}' panicked at server/src/${mod}.rs:${40 + (h % 160)}:9:`,
        g,
      );
      err("assertion `left == right` failed", g);
      err("  left: 404", g);
      err(" right: 200", g);
      out(
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
        g,
      );
      out("", g);
      out("failures:", g);
      out(`    ${failTest}`, g);
      out("", g);
      err(
        `test result: FAILED. ${TESTS.length - 1} passed; 1 failed; 0 ignored; finished in 4.12s`,
        g,
      );
      err(
        `error: builder for '${drv("kajiba-tests-0.4.1")}' failed with exit code 101`,
      );
      err(
        "error: 1 dependencies of derivation 'checks.x86_64-linux.tests' failed to build",
      );
    } else {
      out(
        `test result: ok. ${TESTS.length} passed; 0 failed; 0 ignored; finished in 4.02s`,
        g,
      );
      g = "nixos-smoke";
      out(`building '${drv("vm-test-run-kajiba-smoke")}'...`, g);
      out("machine: starting vm", g);
      out(
        "machine # [    0.000000] Linux version 6.12.30 (nixbld@localhost)",
        g,
      );
      out("machine: waiting for unit kajiba-server.service", g);
      out(
        "machine: (finished: waiting for unit kajiba-server.service, in 6.42 seconds)",
        g,
      );
      out("machine: must succeed: curl -sf http://localhost:3000/-/health", g);
      out(
        "machine: (finished: must succeed: curl -sf http://localhost:3000/-/health, in 0.05 seconds)",
        g,
      );
      out("test script finished in 21.47s", g);
      out(`all ${CHECKS.length} checks passed`);
    }
  } else {
    const ok = run.steps.slice(0, 3).every((s) => s.state === "succeeded");
    cmd(`kajiba-ci report --run ${run.id}`);
    out(`commit status ${run.repo}@${short}: ${ok ? "success" : "failure"}`);
    out("notifying ntfy topic kajiba-ci");
    if (failed) {
      err(
        "error: POST https://ntfy.infra/kajiba-ci: connection refused (os error 111)",
      );
      out("retrying in 2s (attempt 2/3)");
      err(
        "error: POST https://ntfy.infra/kajiba-ci: connection refused (os error 111)",
      );
      out("retrying in 4s (attempt 3/3)");
      err("error: giving up after 3 attempts");
    } else out("done");
  }

  // timestamps spread over the step; a running step is cut at "now"
  const running = st.state === "running";
  const before = run.steps.slice(0, step).reduce((a, s) => a + s.duration, 0);
  const elapsed = running
    ? Math.max(1, run.duration - before)
    : Math.max(1, st.duration);
  const total = running ? Math.max(elapsed * 1.9, elapsed + 30) : elapsed;
  const lines: MockLogLine[] = raw.map((l, i) => ({
    ...l,
    t: Math.round(((i + 1) / raw.length) * total * 10) / 10,
  }));
  return running
    ? { lines: lines.filter((l) => l.t <= elapsed), complete: false }
    : { lines, complete: true };
}
