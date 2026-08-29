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

          # meikiocr is the only project dependency missing from Nixpkgs.
          meikiocr = python.pkgs.buildPythonPackage rec {
            pname = "meikiocr";
            version = "0.3.4";
            pyproject = true;

            src = pkgs.fetchPypi {
              inherit pname version;
              hash = "sha256-dt4dJsLE3BQTIr59DlpUr5jgUsqM/Lrv0okfNy4TRPE=";
            };

            build-system = [ python.pkgs.setuptools ];
            dependencies = with python.pkgs; [
              huggingface-hub
              numpy
              onnxruntime
              opencv4
            ];

            # Nixpkgs' opencv4 provides the cv2 module. Only the Python
            # distribution name differs from the PyPI headless wheel.
            pythonRemoveDeps = [ "opencv-python-headless" ];
            pythonImportsCheck = [ "meikiocr" ];
          };

          pythonEnv = python.withPackages (
            ps: with ps; [
              betterproto
              lxml
              meikiocr
              mss
              pillow
              platformdirs
              protobuf
              pygobject3
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
            exec ${pythonEnv}/bin/python -m meikipop.main "$@"
          '';

          pipewireGstPlugin = "${pkgs.lib.getLib pkgs.pipewire}/lib/gstreamer-1.0";
        in
        {
          default = pkgs.mkShell {
            packages = [
              pythonEnv
              meikipopDev

              # Wayland ScreenCast portal / PipeWire support.
              pkgs.gobject-introspection
              pkgs.gst_all_1.gstreamer
              pkgs.gst_all_1.gst-plugins-base
              pkgs.gst_all_1.gst-plugins-good
              pkgs.gst_all_1.gst-plugins-bad
              pkgs.pipewire

              # Native-extension and general development tools.
              pkgs.pkg-config
              pkgs.ruff
            ];

            shellHook = ''
              export PYTHONPATH="$PWD/src''${PYTHONPATH:+:$PYTHONPATH}"
              export GST_PLUGIN_SYSTEM_PATH_1_0="${pipewireGstPlugin}''${GST_PLUGIN_SYSTEM_PATH_1_0:+:$GST_PLUGIN_SYSTEM_PATH_1_0}"

              echo "meikipop development shell"
              echo "  Run: meikipop"
              echo "  Source: $PWD/src"
            '';
          };
        }
      );
    };
}
