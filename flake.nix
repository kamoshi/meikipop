{
  description = "Development shell for meikipop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
  inputs.nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, nixpkgs-unstable, ... }:
    let
      linuxSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      darwinSystems = [
        "aarch64-darwin"
      ];

      forSystems = systems: f: nixpkgs.lib.genAttrs systems f;

      commonPackages = pkgs: unstablePkgs: [
        # Temporarily needed by the embedded pickle-to-JSON converter.
        pkgs.python312
        (pkgs.writeShellScriptBin "meikipop" ''
          exec cargo run --manifest-path apps/gui-slint/Cargo.toml -- "$@"
        '')

        # Native-extension and general development tools.
        pkgs.pkg-config
        pkgs.openssl.dev

        unstablePkgs.cargo
        unstablePkgs.clippy
        unstablePkgs.rust-analyzer
        unstablePkgs.rustc
        unstablePkgs.rustfmt
      ];

      mkLinuxShell =
        system: withCuda:
        let
          # CUDA is unfree. Restrict that allowance to the opt-in CUDA shell so
          # ordinary development stays independent of the user's Nix policy.
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = withCuda;
          };
          unstablePkgs = import nixpkgs-unstable { inherit system; };
          cudaPackages = pkgs.cudaPackages_13;

          linuxPackages = [
            # Wayland ScreenCast portal / PipeWire support.
            pkgs.pipewire

            # Headers and pkg-config metadata used by the native UI and PipeWire.
            pkgs.fontconfig.dev
            pkgs.noto-fonts-cjk-sans
            pkgs.libxkbcommon.dev
            pkgs.wayland.dev
            pkgs.xorg.libX11.dev
            pkgs.xorg.libXcursor.dev
            pkgs.xorg.libXi.dev
            pkgs.xorg.libxcb.dev
          ];

          runtimeLibraries = [
            pkgs.libxkbcommon
            pkgs.wayland
            pkgs.xorg.libX11
            pkgs.xorg.libXcursor
            pkgs.xorg.libXi
            pkgs.xorg.libxcb
          ] ++ pkgs.lib.optionals withCuda [
            cudaPackages.cudatoolkit
            cudaPackages.libcublas
            cudaPackages.cudnn
          ];

        in
        pkgs.mkShell {
          packages = commonPackages pkgs unstablePkgs
            ++ linuxPackages
            ++ pkgs.lib.optionals withCuda runtimeLibraries;

          shellHook = ''
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            export BINDGEN_EXTRA_CLANG_ARGS="-I${pkgs.stdenv.cc.libc.dev}/include ''${BINDGEN_EXTRA_CLANG_ARGS:-}"
            export LD_LIBRARY_PATH="${
              pkgs.lib.makeLibraryPath runtimeLibraries
            }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            ${pkgs.lib.optionalString withCuda ''
              # libcuda comes from the host NVIDIA driver rather than the CUDA
              # toolkit. NixOS exposes the active driver's libraries here.
              if [ -d /run/opengl-driver/lib ]; then
                export LD_LIBRARY_PATH="/run/opengl-driver/lib:''${LD_LIBRARY_PATH}"
              fi
            ''}

            echo "meikipop development shell (Linux${if withCuda then ", CUDA" else ""})"
            echo "  Run: meikipop"
            echo "  Native library: $PWD/crates/native"
            echo "  Rust GUI: cargo run --manifest-path apps/gui-slint/Cargo.toml"
          '';
        };

      mkDarwinShell =
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          unstablePkgs = import nixpkgs-unstable { inherit system; };

          # Rust's linker does not expand the Swift autolink directives stored
          # inside screencapturekit's static bridge archive, so pass the bridge's
          # Swift overlay dependencies explicitly.
          darwinSwiftLinkFlags = pkgs.lib.concatStringsSep " " [
            "-lswiftAVFoundation"
            "-lswiftCompatibilityPacks"
            "-lswiftCore"
            "-lswiftCoreAudio"
            "-lswiftCoreFoundation"
            "-lswiftCoreGraphics"
            "-lswiftCoreImage"
            "-lswiftCoreMIDI"
            "-lswiftCoreMedia"
            "-lswiftDarwin"
            "-lswiftDispatch"
            "-lswiftFoundation"
            "-lswiftIOKit"
            "-lswiftMetal"
            "-lswiftObjectiveC"
            "-lswiftObservation"
            "-lswiftQuartzCore"
            "-lswiftUniformTypeIdentifiers"
            "-lswiftXPC"
            "-lswift_Concurrency"
            "-lswift_StringProcessing"
            "-lswiftos"
            "-lswiftsimd"
          ];

          darwinSwift = pkgs.writeShellScriptBin "swift" ''
            if [ "$1" = "build" ]; then
              shift
              exec ${unstablePkgs.swift}/bin/swift build \
                --triple arm64-apple-macosx14.0 "$@"
            fi

            exec ${unstablePkgs.swift}/bin/swift "$@"
          '';

          darwinRustLinker = pkgs.writeShellScriptBin "meikipop-rust-linker" ''
            exec ${pkgs.stdenv.cc}/bin/cc "$@" \
              -L"$SDKROOT/usr/lib/swift" ${darwinSwiftLinkFlags}
          '';
        in
        pkgs.mkShell {
          packages = commonPackages pkgs unstablePkgs ++ [
            # ScreenCaptureKit 1.4.x is compatible with this Swift 5.10 / SDK
            # 14.4 toolchain. Keep SwiftPM alongside the compiler because the
            # crate builds a small native bridge package.
            darwinSwift
            unstablePkgs.swift
            unstablePkgs.swiftPackages.swiftpm
          ];

          shellHook = ''
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            export MACOSX_DEPLOYMENT_TARGET="14.0"
            export PATH="${darwinSwift}/bin:$PATH"
            export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="${darwinRustLinker}/bin/meikipop-rust-linker"

            echo "meikipop development shell (macOS)"
            echo "  Run: meikipop"
            echo "  Native library: $PWD/crates/native"
            echo "  Rust GUI: cargo run --manifest-path apps/gui-slint/Cargo.toml"
          '';
        };
    in
    {
      devShells =
        (forSystems linuxSystems (system: {
          default = mkLinuxShell system false;
        } // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
          cuda = mkLinuxShell system true;
        }))
        // (forSystems darwinSystems (system: {
          default = mkDarwinShell system;
        }));
    };
}
