# J1 task 30: the unit's journal passes nisshi with start, ready and request lines (request_id on
# every request), the loopback metrics endpoint passes masu and counts a request from another
# node, and that endpoint is unreachable from the other node even with its port open (0015, 0022).
{
  pkgs,
  kajibaServerPackage,
  kajibaGitPackage,
  testLib,
}:
pkgs.testers.runNixOSTest {
  name = "kajiba-server-observability";

  nodes = {
    server = {
      imports = [ ../nixosModules/server.nix ];
      services.kajiba-git.package = kajibaGitPackage;
      services.kajiba-server = {
        enable = true;
        package = kajibaServerPackage;
        http = {
          listen = "0.0.0.0:8080";
          publicUrl = "https://forge.example.org";
        };
        metrics.listen = "127.0.0.1:9431";
      };
      # 9431 is open too: only the loopback bind can keep the other node out.
      networking.firewall.allowedTCPPorts = [
        8080
        9431
      ];
      environment.systemPackages = [ pkgs.curl ];
    };
    client.environment.systemPackages = [ pkgs.curl ];
  };

  testScript = testLib + ''
    import json
    import re
    import time

    unit = "kajiba-server.service"
    metrics_url = "http://127.0.0.1:9431/metrics"
    series = 'kajiba_requests_total{kind="other",code="2xx"}'


    def requests_2xx():
        text = server.succeed(f"curl -sf {metrics_url}")
        match = re.search(rf"^{re.escape(series)} (\d+)$", text, re.MULTILINE)
        assert match, f"{series} missing from {metrics_url}: {text}"
        return int(match.group(1))


    start_all()

    with subtest("the server is ready through the module"):
        server.wait_for_unit(unit)
        state = server.succeed(f"systemctl show -P ActiveState {unit}").strip()
        assert state == "active", f"ActiveState: {state!r}"

    with subtest("a request from the other node moves kajiba_requests_total"):
        before = requests_2xx()
        client.wait_for_unit("multi-user.target")
        code = client.succeed("curl -s -o /dev/null -w '%{http_code}' http://server:8080/healthz")
        assert code == "200", f"client /healthz: {code}"
        # The counter is updated once the response body is done: allow it a moment.
        after = before
        for _ in range(20):
            after = requests_2xx()
            if after > before:
                break
            time.sleep(0.5)
        assert after > before, f"{series} did not move: {before} -> {after}"

    with subtest("the loopback metrics endpoint passes masu"):
        validate_metrics(server, metrics_url)

    with subtest("the metrics endpoint is not reachable from the other node"):
        status, out = client.execute("curl -s -o /dev/null --max-time 5 http://server:9431/metrics")
        # curl exit 7: connection refused, i.e. nothing listens on the external address.
        assert status == 7, f"client curl to server:9431: exit {status}, output {out!r}"

    with subtest("the journal passes nisshi with start, ready and request lines"):
        validate_logs(server, unit)
        validate_logs(server, "kajiba-git.service")
        out = server.succeed(f"journalctl _SYSTEMD_UNIT={unit} -o cat --no-pager")
        lines = [json.loads(line) for line in out.splitlines()]
        events = {line["event"] for line in lines}
        missing = {"start", "ready", "request"} - events
        assert not missing, f"missing {missing} in events {events}: {out}"
        requests = [line for line in lines if line["event"] == "request"]
        without = [line for line in requests if not line.get("request_id")]
        assert not without, f"request lines without request_id: {without}"
        print(f"{len(requests)} request lines, all with request_id: {[(r.get('method'), r['request_id']) for r in requests]}")
  '';
}
