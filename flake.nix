{
  description = "Development shell for meikipop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
  inputs.nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, nixpkgs-unstable, ... }:
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
          unstablePkgs = import nixpkgs-unstable { inherit system; };
          python = pkgs.python312;

          pythonEnv = python.withPackages (
            ps: with ps; [
              lxml
              mss
              pillow
              platformdirs
              pynput
              pyqt6
              requests
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
              pkgs.fontconfig.dev
              pkgs.noto-fonts-cjk-sans
              pkgs.gst_all_1.gstreamer.dev
              pkgs.gst_all_1.gst-plugins-base.dev
              pkgs.libxkbcommon.dev
              pkgs.wayland.dev
              pkgs.xorg.libX11.dev
              pkgs.xorg.libXcursor.dev
              pkgs.xorg.libXi.dev
              pkgs.xorg.libxcb.dev

              # Native-extension and general development tools.
              unstablePkgs.cargo
              unstablePkgs.clippy
              pkgs.llvmPackages.clang
              pkgs.llvmPackages.libclang
              pkgs.maturin
              pkgs.opencv
              pkgs.openssl.dev
              pkgs.pkg-config
              pkgs.rust-analyzer
              unstablePkgs.rustc
              unstablePkgs.rustfmt
              pkgs.ruff
              pkgs.uv
            ];

            shellHook = ''
              export PYTHONPATH="$PWD/src''${PYTHONPATH:+:$PYTHONPATH}"
              export GST_PLUGIN_SYSTEM_PATH_1_0="${pipewireGstPlugin}''${GST_PLUGIN_SYSTEM_PATH_1_0:+:$GST_PLUGIN_SYSTEM_PATH_1_0}"
              export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
                pkgs.libxkbcommon
                pkgs.wayland
                pkgs.xorg.libX11
                pkgs.xorg.libXcursor
                pkgs.xorg.libXi
                pkgs.xorg.libxcb
              ]}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

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
