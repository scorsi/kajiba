# Shared nixosTest helper (0015, 0022): Python prepended to each J1 testScript, asserting the
# nisshi log schema on a unit's journal (or any command's output) and the masu metrics schema on an HTTP endpoint.
{
  pkgs,
  nisshi,
  masu,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;
in
''
  import shlex


  def validate_log_output(machine, command):
      pipeline = f"{command} | ${nisshi.lib.validateLogs.${system}} validate --logs"
      machine.succeed(f"bash -o pipefail -c {shlex.quote(pipeline)}")


  def validate_logs(machine, unit):
      machine.succeed("journalctl --sync")
      # Select process output: -u also includes systemd's non-JSON status messages.
      validate_log_output(machine, f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")


  def validate_metrics(machine, url):
      machine.succeed(f"${masu.lib.validateMetrics.${system}} validate --metrics {shlex.quote(url)}")


  # J2 repository fixture: main holds flake.nix and README.md, an annotated tag v1 points at
  # main, branch feature/x changes README.md. Built by root in /root/demo; the bundle goes to
  # `bundle`, a world-readable path outside /tmp and /home (the unit has PrivateTmp, ProtectHome).
  DEMO_FLAKE = "{ outputs = _: { }; }\n"


  def make_demo(machine, bundle):
      directory = bundle.rsplit("/", 1)[0]
      script = f"""
          set -eu
          export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
          export GIT_AUTHOR_NAME=alice GIT_AUTHOR_EMAIL=alice@example.org GIT_AUTHOR_DATE=2026-01-01T00:00:00Z
          export GIT_COMMITTER_NAME=alice GIT_COMMITTER_EMAIL=alice@example.org GIT_COMMITTER_DATE=2026-01-01T00:00:00Z
          rm -rf /root/demo
          git init -q -b main /root/demo
          cd /root/demo
          printf %s {shlex.quote(DEMO_FLAKE)} > flake.nix
          echo 'main readme' > README.md
          git add -A
          git commit -q -m main
          git tag -a v1 -m v1
          git checkout -q -b feature/x
          echo 'feature readme' > README.md
          git commit -q -am feature
          git checkout -q main
          mkdir -p -m 0755 {shlex.quote(directory)}
          git bundle create {shlex.quote(bundle)} --all
          chmod 0644 {shlex.quote(bundle)}
      """
      machine.succeed(f"bash -c {shlex.quote(script)}")
      return refs(machine, "/root/demo/.git")


  def refs(machine, git_dir, user="root"):
      # {refname: object id} of every ref but HEAD.
      out = machine.succeed(f"runuser -u {user} -- git --git-dir={shlex.quote(git_dir)} for-each-ref --format='%(refname) %(objectname)'")
      return dict(line.split(" ") for line in out.splitlines())


  def head(machine, git_dir, user="root"):
      return machine.succeed(f"runuser -u {user} -- git --git-dir={shlex.quote(git_dir)} symbolic-ref HEAD").strip()
''
