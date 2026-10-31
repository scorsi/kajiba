# The runner on macOS (0008, 0015): the same options as nixosModules.runner, run as a launchd
# daemon. Darwin execution is J13.
# shortcut: the deployment creates the kajiba-runner account and stateDir (nix-darwin needs a
# fixed uid to manage users); J13 decides whether the module owns them.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.kajiba-runner;
  common = import ../runnerCommon.nix { inherit config lib pkgs; };
in
{
  inherit (common) options;

  config = lib.mkIf cfg.enable {
    environment.etc."kajiba/runner.toml".source = common.configFile;
    launchd.daemons.kajiba-runner.serviceConfig = {
      ProgramArguments = [
        (lib.getExe cfg.package)
        "--config"
        "${common.configFile}"
      ];
      UserName = "kajiba-runner";
      GroupName = "kajiba-runner";
      RunAtLoad = true;
      KeepAlive = true;
      EnvironmentVariables = {
        PATH = lib.makeBinPath common.path;
        # Nix keeps its fetcher and evaluation caches under $HOME.
        HOME = cfg.stateDir;
      };
      StandardErrorPath = "/var/log/kajiba-runner.log";
    };
  };
}
