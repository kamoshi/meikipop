{
  description = "Development shell for meikipop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
  inputs.nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, nixpkgs-unstable, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          unstablePkgs = import nixpkgs-unstable { inherit system; };
          python = pkgs.python312;

          meikipopDev = pkgs.writeShellScriptBin "meikipop" ''
            exec cargo run --manifest-path gui/Cargo.toml -- "$@"
          '';

          linuxPackages = pkgs.lib.optionals pkgs.stdenv.isLinux [
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
          ];

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

          darwinSwiftArch =
            if pkgs.stdenv.hostPlatform.isAarch64 then "arm64" else "x86_64";

          darwinCargoTarget =
            if pkgs.stdenv.hostPlatform.isAarch64
            then "AARCH64_APPLE_DARWIN"
            else "X86_64_APPLE_DARWIN";

          darwinSwift = pkgs.writeShellScriptBin "swift" ''
            if [ "$1" = "build" ]; then
              shift
              exec ${unstablePkgs.swift}/bin/swift build \
                --triple ${darwinSwiftArch}-apple-macosx14.0 "$@"
            fi

            exec ${unstablePkgs.swift}/bin/swift "$@"
          '';

          darwinRustLinker = pkgs.writeShellScriptBin "meikipop-rust-linker" ''
            exec ${pkgs.stdenv.cc}/bin/cc "$@" \
              -L"$SDKROOT/usr/lib/swift" ${darwinSwiftLinkFlags}
          '';

          darwinPackages = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            # ScreenCaptureKit 1.4.x is compatible with this Swift 5.10 / SDK
            # 14.4 toolchain. Keep SwiftPM alongside the compiler because the
            # crate builds a small native bridge package.
            darwinSwift
            unstablePkgs.swift
            unstablePkgs.swiftPackages.swiftpm
          ];

          pipewireGstPlugin = pkgs.lib.optionalString pkgs.stdenv.isLinux
            "${pkgs.lib.getLib pkgs.pipewire}/lib/gstreamer-1.0";
        in
        {
          default = pkgs.mkShell {
            packages = [
              # Temporarily needed by the embedded pickle-to-JSON converter.
              python
              meikipopDev

              # Native-extension and general development tools.
              unstablePkgs.cargo
              unstablePkgs.clippy
              pkgs.llvmPackages.clang
              pkgs.llvmPackages.libclang
              pkgs.opencv
              pkgs.openssl.dev
              pkgs.pkg-config
              pkgs.rust-analyzer
              unstablePkgs.rustc
              unstablePkgs.rustfmt
            ] ++ linuxPackages ++ darwinPackages;

            shellHook = ''
              export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
              ${pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
              export MACOSX_DEPLOYMENT_TARGET="14.0"
              export PATH="${darwinSwift}/bin:$PATH"
              export CARGO_TARGET_${darwinCargoTarget}_LINKER="${darwinRustLinker}/bin/meikipop-rust-linker"
              ''}
              ${pkgs.lib.optionalString pkgs.stdenv.isLinux ''
              export GST_PLUGIN_SYSTEM_PATH_1_0="${pipewireGstPlugin}''${GST_PLUGIN_SYSTEM_PATH_1_0:+:$GST_PLUGIN_SYSTEM_PATH_1_0}"
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
                pkgs.libxkbcommon
                pkgs.wayland
                pkgs.xorg.libX11
                pkgs.xorg.libXcursor
                pkgs.xorg.libXi
                pkgs.xorg.libxcb
              ]}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
              ''}

              echo "meikipop development shell"
              echo "  Run: meikipop"
              echo "  Native library: $PWD/native"
              echo "  Rust GUI: cargo run --manifest-path gui/Cargo.toml"
            '';
          };
        }
      );
    };
}
