# kajiba exports NixOS and Darwin modules for the deployment repository (0015).
{
  flake = {
    nixosModules = {
      server = ./nixosModules/server.nix;
      runner = ./nixosModules/runner.nix;
    };
    darwinModules.runner = ./darwinModules/runner.nix;
  };
}
