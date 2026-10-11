# kajiba exports a flakeModule-shaped set of nixosModules/darwinModules, like the other repos
# in the ecosystem (0015): sekkeizu will import these once it hosts kajiba (J9).
{
  flake = {
    nixosModules = {
      server = ./nixosModules/server.nix;
      runner = ./nixosModules/runner.nix;
    };
    darwinModules.runner = ./darwinModules/runner.nix;
  };
}
