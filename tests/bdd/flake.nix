{
  description = "Development shell for the isolated pg-attach BDD harness";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems
        (system: f nixpkgs.legacyPackages.${system});
    in {
      devShells = forAllSystems (pkgs:
        let
          bddShell = version: postgresql: pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              postgresql
              rustc
              rustfmt
            ];
            shellHook = ''
              export PG_VERSION=${version}
              export PG_CONFIG=${postgresql}/bin/pg_config
            '';
          };
        in {
          default = bddShell "17" pkgs.postgresql_17;
          pg16 = bddShell "16" pkgs.postgresql_16;
          pg17 = bddShell "17" pkgs.postgresql_17;
          pg18 = bddShell "18" pkgs.postgresql_18;
        });
    };
}
