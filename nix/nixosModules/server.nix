# Trivial service module for J0: start kajiba-server, nothing else. stateDir, hardening,
# sops options and the sshd instance land in J1/J3 (0015, 0019, 0022).
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.kajiba-server;
  # shortcut: example configuration until J1 task 25 generates the typed TOML file.
  configFile = pkgs.writeText "kajiba-server.toml" ''
    [http]
    public_url = "https://example.org"
  '';
in
{
  options.services.kajiba-server = {
    enable = lib.mkEnableOption "the kajiba forge server";
    package = lib.mkPackageOption pkgs "kajiba-server" { };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.kajiba-server = {
      description = "kajiba forge server";
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve --config ${configFile}";
        DynamicUser = true;
        User = "kajiba";
        RuntimeDirectory = "kajiba";
        StateDirectory = "kajiba";
        UMask = "0027";
        Restart = "on-failure";
      };
    };
  };
}
