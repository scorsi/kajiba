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
''
