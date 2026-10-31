# J3 task 28: real SSH authentication, signed writes, policy and split-owner reads.
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
let
  # A declared repo:read PAT: RepoService calls authenticate (J4 task 5); a test value, not a secret.
  pat = "kjb_nixosTestTokenNotASecret0000000000000000000";
in
pkgs.testers.runNixOSTest {
  name = "kajiba-git-ssh";

  nodes.forge = {
    imports = [ ../nixosModules/server.nix ];
    services.kajiba-git = {
      package = kajibaGitPackage;
      metrics.listen = "127.0.0.1:9433";
    };
    services.kajiba-server = {
      enable = true;
      package = kajibaServerPackage;
      ui.enable = false;
      http.publicUrl = "https://forge.example.org";
      http.reflection = true;
      metrics.listen = "127.0.0.1:9431";
      users = {
        owner = { };
        writer = {
          sshKeys = [ (builtins.readFile ./fixtures/ssh-writer-key.pub) ];
          signingKeys = [ (builtins.readFile ./fixtures/ssh-signing-key.pub) ];
        };
        reader = {
          sshKeys = [ (builtins.readFile ./fixtures/ssh-reader-key.pub) ];
          tokens.test = {
            sha256 = builtins.hashString "sha256" pat;
            scopes = [ "repo:read" ];
          };
        };
        admin = {
          sshKeys = [ (builtins.readFile ./fixtures/ssh-admin-key.pub) ];
          signingKeys = [ (builtins.readFile ./fixtures/ssh-admin-key.pub) ];
        };
      };
      repos = {
        demo = {
          owner = "owner";
          description = "Signed SSH pushes";
          previousNames = [ "owner/old" ];
          permissions = {
            writer = "write";
            reader = "read";
            admin = "admin";
          };
        };
        empty = {
          owner = "owner";
          description = "First-push ancestor validation";
          permissions.writer = "write";
        };
      };
    };
    specialisation.archived.configuration.services.kajiba-server.repos = pkgs.lib.mkForce { };
    environment.systemPackages = [
      pkgs.git
      pkgs.openssh
      pkgs.sqlite
      pkgs.curl
      pkgs.grpcurl
    ];
  };

  testScript =
    { nodes, ... }:
    testLib
    + ''
      import base64
      import json
      import re

      unit = "kajiba-server.service"
      db = "/var/lib/kajiba-server/db/kajiba.sqlite"
      base = "http://127.0.0.1:8080"
      metrics_url = "http://127.0.0.1:9431/metrics"
      git_metrics_url = "http://127.0.0.1:9433/metrics"
      zero = "0" * 40

      def sql(query):
          return forge.succeed(f"sqlite3 {db} {shlex.quote(query)}").strip()

      def events():
          out = forge.succeed(f"sqlite3 -json {db} \"SELECT * FROM event WHERE kind='push' ORDER BY id\"")
          return json.loads(out or "[]")

      def metric(url, series):
          lines = forge.succeed(f"curl -fsS {url}").splitlines()
          return float(next(line.split()[-1] for line in lines if line.startswith(series + " ")))

      def settle():
          forge.wait_until_succeeds(f"test $(sqlite3 {db} 'SELECT COUNT(*) FROM pending_push') = 0")
          for kind in ["receive_pack", "upload_pack"]:
              series = f'kajiba_git_sessions_active{{kind="{kind}"}} 0'
              forge.wait_until_succeeds(f"curl -fsS {git_metrics_url} | grep -Fx {shlex.quote(series)}")

      def ssh(user):
          return f"ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/known_hosts -o ConnectTimeout=5 -i /root/{user}-key -p 2222"

      def git(args, user="writer", success=True):
          command = f"env GIT_SSH_COMMAND={shlex.quote(ssh(user))} git {args}"
          if success:
              return forge.succeed(command)
          code, out = forge.execute(command + " 2>&1")
          assert code != 0, out
          return out

      def remote_refs(repo="demo"):
          return refs(forge, directories[repo], user="kajiba-git")

      def push(source, ref, user="writer", repo="demo", reason=None, force=False):
          settle()
          before, journal = remote_refs(repo), events()
          accepted = metric(metrics_url, 'kajiba_pushes_total{result="accepted"}')
          refused = metric(metrics_url, 'kajiba_pushes_total{result="refused"}')
          new = git(f"-C /root/work rev-parse {source}").strip() if source else zero
          option = "--force" if force else ""
          out = git(f"-C /root/work push {option} kajiba-git@127.0.0.1:owner/{repo}.git {source}:{ref}", user, success=reason is None)
          settle()
          after = events()
          if reason is not None:
              assert reason in out, out
              assert remote_refs(repo) == before
              assert after == journal
              assert metric(metrics_url, 'kajiba_pushes_total{result="accepted"}') == accepted
              assert metric(metrics_url, 'kajiba_pushes_total{result="refused"}') == refused + 1
              return out
          expected = dict(before)
          if source:
              expected[ref] = new
          else:
              del expected[ref]
          assert remote_refs(repo) == expected
          assert after[:-1] == journal and len(after) == len(journal) + 1, after
          event = after[-1]
          assert event["kind"] == "push" and event["version"] == 1
          assert event["principal_kind"] == "user" and event["principal_id"] == int(sql(f"SELECT id FROM user WHERE name='{user}'"))
          assert event["repo_id"] == int(sql(f"SELECT id FROM repo WHERE config_key='{repo}'"))
          for field in ["public_id", "correlation_id"]:
              assert re.fullmatch(r"[0-7][0-9A-HJKMNP-TV-Z]{25}", event[field]), event
          assert event["id"] > 0 and event["occurred_at"] > 0
          assert json.loads(event["payload"]) == {"ref": ref, "old": before.get(ref, zero), "new": new}
          assert metric(metrics_url, 'kajiba_pushes_total{result="accepted"}') == accepted + 1
          assert metric(metrics_url, 'kajiba_pushes_total{result="refused"}') == refused
          return event

      def rpc(method, **request):
          data = shlex.quote(json.dumps(request))
          return json.loads(forge.succeed(f"grpcurl -plaintext -H 'authorization: Bearer ${pat}' -d {data} 127.0.0.1:8080 kajiba.v1.RepoService/{method}"))

      forge.wait_for_unit(unit)
      forge.wait_for_unit("kajiba-git.service")
      forge.wait_for_unit("kajiba-sshd.service")
      forge.wait_until_succeeds(f"curl -fsS {base}/readyz")
      directories = {}
      for key in ["demo", "empty"]:
          public_id = sql(f"SELECT public_id FROM repo WHERE config_key='{key}'")
          directories[key] = f"/var/lib/kajiba-git/repos/{public_id}.git"
      # Public test credentials only; private keys never belong in deployment configuration.
      forge.succeed(
          "install -m 600 ${./fixtures/ssh-writer-key} /root/writer-key",
          "install -m 600 ${./fixtures/ssh-reader-key} /root/reader-key",
          "install -m 600 ${./fixtures/ssh-admin-key} /root/admin-key",
          "install -m 600 ${./fixtures/ssh-signing-key} /root/signing-key",
          'ssh-keygen -q -t ed25519 -N "" -f /root/unknown-key',
          "ssh-keyscan -p 2222 127.0.0.1 > /root/known_hosts",
          "git init -b main /root/work",
          "git -C /root/work config user.name Writer",
          "git -C /root/work config user.email writer@example.org",
          "git -C /root/work config gpg.format ssh",
          "git -C /root/work config user.signingkey /root/signing-key",
          "echo 'signed content' > /root/work/README.md",
          "git -C /root/work add README.md",
          "git -C /root/work commit -S -m initial",
      )
      initial = git("-C /root/work rev-parse HEAD").strip()

      with subtest("write user signed push and durable event envelope through the dedicated sshd"):
          assert metric(metrics_url, 'kajiba_pushes_total{result="accepted"}') == 0
          assert metric(git_metrics_url, 'kajiba_git_sessions_total{kind="receive_pack",result="ok"}') == 0
          push(initial, "refs/heads/main")
          assert metric(git_metrics_url, 'kajiba_git_sessions_total{kind="receive_pack",result="ok"}') == 1

      with subtest("promoted objects are git-owned, group-readable, never group-writable and readable by the server"):
          directory = directories["demo"]
          entries = forge.succeed(f"find {directory} -printf '%u %g %m %y %p\\n'").splitlines()
          objects = []
          for entry in entries:
              owner, group, mode, kind, path = entry.split(" ", 4)
              assert (owner, group) == ("kajiba-git", "kajiba-git"), entry
              assert kind in ["d", "f"], entry
              assert int(mode, 8) in ([0o750] if kind == "d" else [0o640, 0o440]), entry
              if kind == "f" and "/objects/" in path:
                  objects.append(path)
                  forge.succeed(f"runuser -u kajiba-server -- test -r {path}")
          assert len(objects) >= 3, entries
          forge.succeed(f"runuser -u kajiba-server -- git -c safe.directory={directory} --git-dir={directory} cat-file -p {initial}")
          tree = rpc("GetTree", repo="owner/demo", revision=initial)
          assert tree["commitId"] == initial and tree["entries"][0]["name"] == "README.md"
          blob = rpc("GetBlob", repo="owner/demo", revision=initial, path="README.md")
          assert blob["commitId"] == initial and base64.b64decode(blob["content"]) == b"signed content\n"
          assert rpc("ListCommits", repo="owner/demo", revision="main")["commits"][0]["id"] == initial
          forge.succeed(f"git clone {base}/owner/demo.git /root/http-clone")
          assert git("-C /root/http-clone rev-parse HEAD").strip() == initial
          assert forge.succeed("cat /root/http-clone/README.md") == "signed content\n"

      with subtest("SSH clone and fetch through current and redirected addresses"):
          git("clone kajiba-git@127.0.0.1:owner/old.git /root/clone", user="reader")
          git("clone kajiba-git@127.0.0.1:owner/demo /root/clone-current", user="reader")
          for directory in ["/root/clone", "/root/clone-current"]:
              assert git(f"-C {directory} rev-parse HEAD").strip() == initial
          git("-C /root/work commit --allow-empty -S -m next")
          tip = git("-C /root/work rev-parse HEAD").strip()
          push(tip, "refs/heads/main")
          for address in ["owner/demo", "/owner/old.git"]:
              git(f"-C /root/clone fetch kajiba-git@127.0.0.1:{address} main", user="reader")
              assert git("-C /root/clone rev-parse FETCH_HEAD").strip() == tip
          settle()
          assert metric(git_metrics_url, 'kajiba_git_sessions_total{kind="upload_pack",result="ok"}') == 4

      with subtest("unsigned and wrong-signer commits name the refused ref and object"):
          for signing in ["--no-gpg-sign", "-S/root/admin-key"]:
              git(f"-C /root/work commit --allow-empty {signing} -m rejected")
              bad = git("-C /root/work rev-parse HEAD").strip()
              out = push(bad, "refs/heads/main", reason="SSH signature")
              assert bad in out and "refs/heads/main" in out, out
              git(f"-C /root/work reset --hard {tip}")

      with subtest("a signed first-push tip cannot hide an unsigned ancestor"):
          tree = git(f"-C /root/work rev-parse {tip}^{{tree}}").strip()
          ancestor = git(f"-C /root/work commit-tree {tree} -m unsigned-root").strip()
          signed = git(f"-C /root/work commit-tree -S {tree} -p {ancestor} -m signed-tip").strip()
          out = push(signed, "refs/heads/main", repo="empty", reason="SSH signature")
          assert ancestor in out and "refs/heads/main" in out, out
          assert remote_refs("empty") == {}

      with subtest("lightweight tags fail and signed annotated tags succeed"):
          git(f"-C /root/work tag light {tip}")
          out = push("refs/tags/light", "refs/tags/light", reason="annotated tag with a valid SSH signature")
          assert "refs/tags/light" in out and tip in out, out
          git(f"-C /root/work tag -s v1 {tip} -m release")
          push("refs/tags/v1", "refs/tags/v1")

      with subtest("read users cannot push and sshd refuses an unknown auth key"):
          push(tip, "refs/heads/read-denied", user="reader", reason="repository role required")
          before, journal = remote_refs(), events()
          out = git("-C /root/work push kajiba-git@127.0.0.1:owner/demo main", user="unknown", success=False)
          assert "Permission denied (publickey)" in out, out
          assert remote_refs() == before and events() == journal

      with subtest("untrusted ref prefixes are rejected by pre-receive"):
          for ref in ["refs/contrib/writer/topic", "refs/foo"]:
              out = push(tip, ref, reason="only refs/heads/* and refs/tags/* may be pushed")
              assert ref in out, out

      with subtest("only repository admins may force-push or delete, and no one may delete the default branch"):
          push(initial, "refs/heads/main", force=True, reason="non-fast-forward update requires repository admin role")
          push(initial, "refs/heads/main", user="admin", force=True)
          push("", "refs/heads/main", reason="deleting the default branch is forbidden")
          push("", "refs/heads/main", user="admin", reason="deleting the default branch is forbidden")
          push(tip, "refs/heads/topic")
          push(initial, "refs/heads/topic", force=True, reason="non-fast-forward update requires repository admin role")
          push(initial, "refs/heads/topic", user="admin", force=True)
          for ref in ["refs/heads/topic", "refs/tags/v1"]:
              push("", ref, reason="deleting a ref requires repository admin role")
              push("", ref, user="admin")
          push(tip, "refs/heads/main")

      with subtest("SSH rejects commands outside the Git transport grammar"):
          before, journal = remote_refs(), events()
          for command in ["id", "sh -c 'id'", "git-upload-pack '../demo'", "git-upload-pack 'owner/demo' extra", "git-receive-pack 'owner/demo'; id"]:
              code, out = forge.execute(f"{ssh('writer')} kajiba-git@127.0.0.1 {shlex.quote(command)} </dev/null 2>&1")
              assert code != 0 and "invalid SSH command" in out, out
          settle()
          assert remote_refs() == before and events() == journal

      with subtest("readiness tracks the dedicated sshd and recovers without restarting either daemon"):
          forge.succeed("systemctl stop kajiba-sshd.service")
          forge.wait_until_succeeds(f"test $(curl -s -o /dev/null -w '%{{http_code}}' {base}/readyz) = 503")
          forge.succeed(f"systemctl is-active {unit} kajiba-git.service", "systemctl start kajiba-sshd.service")
          forge.wait_until_succeeds(f"curl -fsS {base}/readyz")

      with subtest("push logs match durable events and both enabled metrics endpoints validate"):
          settle()
          forge.succeed("journalctl --sync")
          logs = [json.loads(line) for line in forge.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager").splitlines()]
          pushes = [line for line in logs if line.get("event") == "push"]
          assert len(pushes) == len(events())
          for log, event in zip(pushes, events()):
              assert log["request_id"] == event["correlation_id"] and log["repo"] == "owner/demo"
              assert log["user"] == sql(f"SELECT name FROM user WHERE id={event['principal_id']}")
              for field, value in json.loads(event["payload"]).items():
                  assert log[field] == value, (log, event)
          assert any(line.get("event") == "push-refused" and line.get("reason") for line in logs)
          assert metric(metrics_url, 'kajiba_pushes_total{result="accepted"}') == len(events())
          assert metric(metrics_url, 'kajiba_pushes_total{result="refused"}') > 0
          validate_metrics(forge, metrics_url)
          validate_metrics(forge, git_metrics_url)

      with subtest("declarative archival refuses SSH clone, fetch and push through both names"):
          before, journal = remote_refs(), events()
          forge.succeed(f"systemctl stop {unit}", "${nodes.forge.system.build.toplevel}/specialisation/archived/bin/switch-to-configuration test", f"systemctl start {unit}")
          forge.wait_for_unit(unit)
          assert sql("SELECT COUNT(*) FROM repo WHERE archived_at IS NOT NULL") == "2"
          for address in ["owner/demo", "owner/old.git"]:
              for args in [f"clone kajiba-git@127.0.0.1:{address} /root/archived", f"-C /root/clone fetch kajiba-git@127.0.0.1:{address}", f"-C /root/work push kajiba-git@127.0.0.1:{address} main"]:
                  assert "archived" in git(args, success=False)
          settle()
          assert remote_refs() == before and events() == journal
          forge.wait_until_succeeds(f"curl -fsS {base}/readyz")
          assert metric(metrics_url, 'kajiba_pushes_total{result="refused"}') == 2

      validate_logs(forge, unit)
      validate_logs(forge, "kajiba-git.service")
      validate_metrics(forge, metrics_url)
      validate_metrics(forge, git_metrics_url)
    '';
}
