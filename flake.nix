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
      }
    ) // {

      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.infi75;

          cavaConfig = pkgs.writeText "infi75-cava.cfg" ''
            [general]
            framerate = 60
            bars = 16
            autosens = 1

            [smoothing]
            monstercat = 1
            # waves = 1
            waveform = 1
            # noise_reduction = 1.5
            gravity = 80

            [output]
            method = raw
            raw_target = /dev/stdout
            bit_format = 8bit
            channels = mono

            [eq]
            1 = 1 # bass
            2 = 1
            3 = 1 # midtone
            4 = 1
            5 = 1 # treble
          '';

          infi75-custom = self.packages.${pkgs.stdenv.hostPlatform.system}.default;

        in
        {
          options.services.infi75 = {
            enable = lib.mkEnableOption "infi75 cava visualizer";
          };

          config = lib.mkIf cfg.enable {
            systemd.user.services.infi75 = {
              description = "infi75 - cava visualizer piped to infi75-custom";
              wantedBy = [ "default.target" ];
              after = [ "sound.target" ];
              wants = [ "sound.target" ];
              serviceConfig = {
                Type = "simple";
                ExecStart = "${pkgs.bash}/bin/sh -c '${pkgs.cava}/bin/cava -p ${cavaConfig} | ${infi75-custom}/bin/infi75-custom -m cava'";
                Restart = "on-failure";
                RestartSec = "3s";
              };
            };
          };
        };

    };
}
