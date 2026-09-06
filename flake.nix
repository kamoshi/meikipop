{
  description = "MeikiPop development environment and runnable application";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
  inputs.nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    {
      self,
      nixpkgs,
      nixpkgs-unstable,
      ...
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];

      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

      packageSetsFor = system: {
        pkgs = import nixpkgs { inherit system; };
        unstablePkgs = import nixpkgs-unstable { inherit system; };
      };

      linuxRuntimeLibraries = pkgs: [
        pkgs.fontconfig
        pkgs.libglvnd
        pkgs.libxkbcommon
        pkgs.pipewire
        pkgs.wayland
        pkgs.xorg.libX11
        pkgs.xorg.libXcursor
        pkgs.xorg.libXi
        pkgs.xorg.libxcb
      ];

      linuxDevelopmentPackages = pkgs: [
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

      mkDarwinToolchain =
        { pkgs, unstablePkgs }:
        let
          # Rust's linker does not expand the Swift autolink directives stored
          # inside screencapturekit's static bridge archive.
          swiftLibraries = [
            "swiftAVFoundation"
            "swiftCompatibilityPacks"
            "swiftCore"
            "swiftCoreAudio"
            "swiftCoreFoundation"
            "swiftCoreGraphics"
            "swiftCoreImage"
            "swiftCoreMIDI"
            "swiftCoreMedia"
            "swiftDarwin"
            "swiftDispatch"
            "swiftFoundation"
            "swiftIOKit"
            "swiftMetal"
            "swiftObjectiveC"
            "swiftObservation"
            "swiftQuartzCore"
            "swiftUniformTypeIdentifiers"
            "swiftXPC"
            "swift_Concurrency"
            "swift_StringProcessing"
            "swiftos"
            "swiftsimd"
          ];
          swiftLinkFlags = pkgs.lib.concatMapStringsSep " " (name: "-l${name}") swiftLibraries;
          swift = pkgs.writeShellScriptBin "swift" ''
            if [ "$1" = "build" ]; then
              shift
              exec ${unstablePkgs.swift}/bin/swift build \
                --triple arm64-apple-macosx14.0 "$@"
            fi

            exec ${unstablePkgs.swift}/bin/swift "$@"
          '';
          linker = pkgs.writeShellScriptBin "meikipop-rust-linker" ''
            exec ${pkgs.stdenv.cc}/bin/cc "$@" \
              -L"$SDKROOT/usr/lib/swift" ${swiftLinkFlags}
          '';
        in
        {
          inherit linker swift;

          packages = [
            # ScreenCaptureKit 1.4.x is compatible with this Swift 5.10 / SDK
            # 14.4 toolchain. SwiftPM builds its small native bridge package.
            swift
            unstablePkgs.swift
            unstablePkgs.swiftPackages.swiftpm
          ];

          environment = {
            MACOSX_DEPLOYMENT_TARGET = "14.0";
            CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER =
              "${linker}/bin/meikipop-rust-linker";
          };
        };

      commonDevelopmentPackages =
        { pkgs, unstablePkgs }:
        [
          # Temporarily needed by the embedded pickle-to-JSON converter.
          unstablePkgs.python3
          (pkgs.writeShellScriptBin "meikipop" ''
            exec cargo run --manifest-path apps/gui-slint/Cargo.toml -- "$@"
          '')

          pkgs.pkg-config
          pkgs.openssl.dev
          unstablePkgs.cargo
          unstablePkgs.clippy
          unstablePkgs.rust-analyzer
          unstablePkgs.rustc
          unstablePkgs.rustfmt
        ];

      mkPackage =
        system:
        let
          packageSets = packageSetsFor system;
          inherit (packageSets) pkgs unstablePkgs;
          isLinux = pkgs.stdenv.hostPlatform.isLinux;
          isDarwin = pkgs.stdenv.hostPlatform.isDarwin;
          onnxruntime = unstablePkgs.onnxruntime;
          runtimeLibraries = linuxRuntimeLibraries pkgs;
          darwinToolchain = mkDarwinToolchain packageSets;
        in
        unstablePkgs.rustPlatform.buildRustPackage {
          pname = "meikipop";
          version = "0.1.0";
          src = self;

          cargoRoot = "apps/gui-slint";
          buildAndTestSubdir = "apps/gui-slint";
          cargoLock.lockFile = ./apps/gui-slint/Cargo.lock;

          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.makeWrapper
            unstablePkgs.rustPlatform.bindgenHook
          ]
          ++ pkgs.lib.optionals isLinux [ pkgs.addDriverRunpath ]
          ++ pkgs.lib.optionals isDarwin darwinToolchain.packages;

          buildInputs = [
            pkgs.openssl
            onnxruntime
          ] ++ pkgs.lib.optionals isLinux runtimeLibraries;

          env = {
            ORT_LIB_LOCATION = "${onnxruntime}";
            ORT_PREFER_DYNAMIC_LINK = "1";
          } // pkgs.lib.optionalAttrs isDarwin darwinToolchain.environment;

          postFixup =
            if isLinux then
              ''
                addDriverRunpath "$out/bin/meikipop-gui"
                wrapProgram "$out/bin/meikipop-gui" \
                  --prefix PATH : "${unstablePkgs.python3}/bin" \
                  --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath runtimeLibraries}"
              ''
            else
              ''
                wrapProgram "$out/bin/meikipop-gui" \
                  --prefix PATH : "${unstablePkgs.python3}/bin"
              '';

          meta = {
            description = "Screen OCR and dictionary popup";
            mainProgram = "meikipop-gui";
            platforms = supportedSystems;
          };
        };

      mkLinuxShell =
        system: withCuda:
        let
          # CUDA is unfree. Restrict that allowance to the opt-in CUDA shell.
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = withCuda;
          };
          unstablePkgs = import nixpkgs-unstable { inherit system; };
          cudaPackages = pkgs.cudaPackages_13;
          runtimeLibraries = linuxRuntimeLibraries pkgs ++ pkgs.lib.optionals withCuda [
            cudaPackages.cudatoolkit
            cudaPackages.libcublas
            cudaPackages.cudnn
          ];
        in
        pkgs.mkShell {
          packages = commonDevelopmentPackages { inherit pkgs unstablePkgs; }
            ++ linuxDevelopmentPackages pkgs
            ++ runtimeLibraries;

          shellHook = ''
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            export BINDGEN_EXTRA_CLANG_ARGS="-I${pkgs.stdenv.cc.libc.dev}/include ''${BINDGEN_EXTRA_CLANG_ARGS:-}"
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibraries}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

            # Hardware-specific OpenGL, Vulkan, and CUDA implementations come
            # from the host driver on NixOS. The loaders dispatch to them.
            if [ -d /run/opengl-driver/lib ]; then
              export LD_LIBRARY_PATH="/run/opengl-driver/lib:''${LD_LIBRARY_PATH}"
            fi

            echo "meikipop development shell (Linux${if withCuda then ", CUDA" else ""})"
            echo "  Run: meikipop"
            echo "  Native library: $PWD/crates/native"
            echo "  Rust GUI: cargo run --manifest-path apps/gui-slint/Cargo.toml"
          '';
        };

      mkDarwinShell =
        system:
        let
          packageSets = packageSetsFor system;
          inherit (packageSets) pkgs;
          toolchain = mkDarwinToolchain packageSets;
        in
        pkgs.mkShell {
          packages = commonDevelopmentPackages packageSets ++ toolchain.packages;

          shellHook = ''
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            export MACOSX_DEPLOYMENT_TARGET="${toolchain.environment.MACOSX_DEPLOYMENT_TARGET}"
            export PATH="${toolchain.swift}/bin:$PATH"
            export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="${toolchain.environment.CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER}"

            echo "meikipop development shell (macOS)"
            echo "  Run: meikipop"
            echo "  Native library: $PWD/crates/native"
            echo "  Rust GUI: cargo run --manifest-path apps/gui-slint/Cargo.toml"
          '';
        };
    in
    {
      packages = forAllSystems (system: {
        default = mkPackage system;
      });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/meikipop-gui";
          meta.description = "Run MeikiPop";
        };
      });

      devShells = forAllSystems (
        system:
        if system == "x86_64-linux" then
          {
            default = mkLinuxShell system false;
            cuda = mkLinuxShell system true;
          }
        else
          {
            default = mkDarwinShell system;
          }
      );
    };
}
