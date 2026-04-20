{
  description = "infi75-custom";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils }:
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "infi75-custom";
          version = "0.1.0";

          src = ./.;

          cargoHash = "sha256-UqG67Gw3zsrY+70ixnfxIO6iHl6g1CjaGRMP1gOr2eY=";

          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.udev ];
        };

        devShells.default = pkgs.mkShell {
          buildInputs = [ 
            pkgs.cargo 
            pkgs.rustc 
            pkgs.rust-analyzer 
            pkgs.pkg-config
            pkgs.udev
          ];
        };
      });
}
