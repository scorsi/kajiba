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
        pusher: i % 7 === 0 ? "mika" : "alice",
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
  inputs.kajiba.url = "git+https://git.example.org/alice/kajiba";

  outputs = { kajiba, ... }: {
    nixosConfigurations.forge = {
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
git clone ssh://kajiba-git@git.example.org:2222/alice/kajiba.git
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
  ".github": t({ CODEOWNERS: b("* @alice\n") }),
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
    url: "https://git.example.org/alice/kajiba-frontend.git",
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
    workstation: t({
      "default.nix": b(
        '{ ... }: {\n  networking.hostName = "workstation";\n}\n',
      ),
    }),
    forge: t({
      "default.nix": b("{ ... }: {\n  services.kajiba.enable = true;\n}\n"),
    }),
  }),
  modules: t({ "kajiba.nix": b("{ config, lib, ... }: { }\n") }),
  "flake.nix": b(
    '{\n  description = "infra — my homelab";\n  outputs = { ... }: { };\n}\n',
  ),
  "README.md": b(
    "# infra\n\nthe blueprint of my homelab: a Mac Mini host and its NixOS guests.\n\n```sh\nnix flake check\n```\n",
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

const ALICE: [string, string] = ["Alice", "alice@kajiba.invalid"];
const MIKA: [string, string] = ["Mika Arai", "mika@kajiba.invalid"];
const NOW = Math.floor(Date.now() / 1000);

// ── repositories ───────────────────────────────────────────────────────

export interface MockRepo {
  publicId: string;
  owner: string;
  name: string;
  description: string;
  defaultBranch: string;
  ciEnabled: boolean;
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
    owner: "alice",
    name: "kajiba",
    description: "A small, declarative, self-hosted git forge.",
    defaultBranch: "refs/heads/main",
    ciEnabled: true,
    previousNames: ["forge"],
    permissions: [
      { user: "alice", role: "admin" },
      { user: "mika", role: "write" },
      { user: "ren", role: "read" },
    ],
    mirrors: [
      {
        remoteUrl: "git@github.com:alice/kajiba.git",
        credential: "github-deploy-key",
        enabled: true,
      },
    ],
    archived: false,
    tree: kajibaTree,
    commits: makeHistory("kajiba", 420, NOW - 3 * 3600, [
      ALICE,
      ALICE,
      ALICE,
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
    owner: "alice",
    name: "infra",
    description: "My homelab: nix-darwin host and NixOS guests.",
    defaultBranch: "refs/heads/main",
    ciEnabled: true,
    previousNames: [],
    permissions: [{ user: "alice", role: "admin" }],
    mirrors: [
      {
        remoteUrl: "git@github.com:alice/infra.git",
        credential: "github-deploy-key",
        enabled: true,
      },
    ],
    archived: false,
    tree: infraTree,
    commits: makeHistory("infra", 64, NOW - 26 * 3600, [ALICE]),
    refs: { "refs/heads/main": 0 },
  },
  {
    publicId: "r_9xq2la",
    owner: "alice",
    name: "hikouki",
    description: "Starship-like toolkit for Claude Code.",
    defaultBranch: "refs/heads/main",
    ciEnabled: false,
    previousNames: [],
    permissions: [{ user: "alice", role: "admin" }],
    mirrors: [
      {
        remoteUrl: "git@github.com:alice/hikouki.git",
        credential: "github-deploy-key",
        enabled: true,
      },
    ],
    archived: false,
    tree: hikoukiTree,
    commits: makeHistory("hikouki", 9, NOW - 9 * 86400, [ALICE]),
    refs: { "refs/heads/main": 0, "refs/tags/v0.1.0": 2 },
  },
  {
    publicId: "r_44pd0z",
    owner: "alice",
    name: "scratch",
    description: "",
    defaultBranch: "refs/heads/main",
    ciEnabled: false,
    previousNames: [],
    permissions: [{ user: "alice", role: "admin" }],
    mirrors: [],
    archived: false,
    tree: t({}),
    commits: [], // empty repository: no refs at all
    refs: {},
  },
  {
    publicId: "r_0c1dot",
    owner: "alice",
    name: "dotfiles",
    description: "Old dotfiles, superseded by infra.",
    defaultBranch: "refs/heads/master",
    ciEnabled: false,
    previousNames: [],
    permissions: [{ user: "alice", role: "admin" }],
    mirrors: [
      {
        remoteUrl: "git@github.com:alice/dotfiles.git",
        credential: "github-deploy-key",
        enabled: false,
      },
    ],
    archived: true,
    tree: dotfilesTree,
    commits: makeHistory("dotfiles", 40, NOW - 400 * 86400, [ALICE]),
    refs: { "refs/heads/master": 0 },
  },
  {
    publicId: "r_m1n0te",
    owner: "mika",
    name: "notes",
    description: "Scratch notes.",
    defaultBranch: "refs/heads/main",
    ciEnabled: false,
    previousNames: [],
    permissions: [
      { user: "mika", role: "admin" },
      { user: "alice", role: "read" },
    ],
    mirrors: [],
    archived: false,
    tree: notesTree,
    commits: makeHistory("notes", 12, NOW - 4 * 86400, [MIKA]),
    refs: { "refs/heads/main": 0 },
  },
];

/** Signed-in user of the mock session. */
export const MOCK_VIEWER = { user: "alice", admin: true };

// ── runners & stats (ASSUMED services, see api.ts) ──────────────────────

export const MOCK_RUNNERS = [
  {
    name: "forge-1",
    host: "forge",
    backend: "nix",
    labels: ["nix", "x86_64-linux"],
    state: "busy" as const,
    lastSeen: NOW - 4,
    currentRun: {
      id: "1287",
      repo: "alice/kajiba",
      ref: "refs/heads/main",
      startedAt: NOW - 140,
    },
  },
  {
    name: "forge-2",
    host: "forge",
    backend: "nspawn",
    labels: ["nix", "x86_64-linux", "nspawn"],
    state: "idle" as const,
    lastSeen: NOW - 9,
  },
  {
    name: "workstation-1",
    host: "workstation",
    backend: "nix",
    labels: ["nix", "aarch64-darwin"],
    state: "idle" as const,
    lastSeen: NOW - 21,
  },
  {
    name: "workstation-vm",
    host: "workstation",
    backend: "microvm",
    labels: ["nix", "aarch64-linux", "microvm"],
    state: "offline" as const,
    lastSeen: NOW - 2 * 86400 - 3600,
  },
];

/** Commits per day over the last 30 days, oldest first. */
const activity = Array.from({ length: 30 }, (_, i) => {
  const d = new Date((NOW - (29 - i) * 86400) * 1000).getUTCDay();
  return d === 0 || d === 6 ? 1 + (hash32("a" + i) % 9) : hash32("a" + i) % 5;
});

export const MOCK_STATS = {
  repositories: MOCK_REPOS.length,
  archived: MOCK_REPOS.filter((r) => r.archived).length,
  commits30d: activity.reduce((a, b) => a + b, 0),
  activity30d: activity,
  runs7d: { succeeded: 38, failed: 4, cancelled: 1 },
  runMedianSeconds: 212,
  mirrors: { ok: 2, failing: 0, disabled: 1 },
  diskBytes: 1_842_000_000,
};

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
  "alice/kajiba git@github.com:alice/kajiba.git": {
    state: "ok",
    lastAttempt: NOW - 3 * 3600,
    lastSuccess: NOW - 3 * 3600,
  },
  "alice/infra git@github.com:alice/infra.git": {
    state: "ok",
    lastAttempt: NOW - 26 * 3600,
    lastSuccess: NOW - 26 * 3600,
  },
  "alice/hikouki git@github.com:alice/hikouki.git": {
    state: "failing",
    lastAttempt: NOW - 600,
    lastSuccess: NOW - 9 * 86400,
    error: "remote: Permission to alice/hikouki.git denied to deploy key.",
    attempts: 7,
    nextAttempt: NOW + 1800,
  },
  "alice/dotfiles git@github.com:alice/dotfiles.git": {
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
  users: ["alice", "mika", "ren"],
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
    name: "kajiba-cli on workstation",
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
    scopes: ["repo:read", "repo:write"],
    created: NOW - 200 * 86400,
    expires: NOW + 165 * 86400,
    lastUsed: NOW - 26 * 3600,
    declared: true,
  },
];
