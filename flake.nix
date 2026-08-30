{
  description = "Development shell for meikipop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";

  outputs =
    { nixpkgs, ... }:
    let
      forAllLinuxSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      devShells = forAllLinuxSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          python = pkgs.python312;

          pythonEnv = python.withPackages (
            ps: with ps; [
              betterproto
              lxml
              mss
              pillow
              platformdirs
              protobuf
              pynput
              pyqt6
              requests
              websockets
              xlib

              # Development tools.
              pytest
            ]
          );

          meikipopDev = pkgs.writeShellScriptBin "meikipop" ''
            exec python -m meikipop.main "$@"
          '';

          pipewireGstPlugin = "${pkgs.lib.getLib pkgs.pipewire}/lib/gstreamer-1.0";
        in
        {
          default = pkgs.mkShell {
            packages = [
              pythonEnv
              meikipopDev

              # Wayland ScreenCast portal / PipeWire support.
              pkgs.gst_all_1.gstreamer
              pkgs.gst_all_1.gst-plugins-base
              pkgs.gst_all_1.gst-plugins-good
              pkgs.gst_all_1.gst-plugins-bad
              pkgs.pipewire

              # Headers and pkg-config metadata used by gstreamer-rs.
              pkgs.gst_all_1.gstreamer.dev
              pkgs.gst_all_1.gst-plugins-base.dev

              # Native-extension and general development tools.
              pkgs.cargo
              pkgs.clippy
              pkgs.llvmPackages.clang
              pkgs.llvmPackages.libclang
              pkgs.maturin
              pkgs.opencv
              pkgs.openssl.dev
              pkgs.pkg-config
              pkgs.rust-analyzer
              pkgs.rustc
              pkgs.rustfmt
              pkgs.ruff
              pkgs.uv
            ];

            shellHook = ''
              export PYTHONPATH="$PWD/src''${PYTHONPATH:+:$PYTHONPATH}"
              export GST_PLUGIN_SYSTEM_PATH_1_0="${pipewireGstPlugin}''${GST_PLUGIN_SYSTEM_PATH_1_0:+:$GST_PLUGIN_SYSTEM_PATH_1_0}"
              export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"

              echo "meikipop development shell"
              echo "  Run: meikipop"
              echo "  Source: $PWD/src"
              echo "  Native install: maturin develop --manifest-path native/Cargo.toml"
            '';
          };
        }
      );
    };
}
