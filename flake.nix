{
  description = "MeikiPop development environment and runnable application";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
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
      flatpakAppId = "org.kamoshi.meikipop";
      flatpakRuntimeVersion = "25.08";
      appName = "MeikiPop";
      appVersion = "0.1.0";
      bundleId = "org.kamoshi.meikipop";
      bundleVersion = "1";
      minimumMacOSVersion = "14.0";

      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

      # Keep the expensive Rust derivation independent from documentation,
      # CI, Swift, Flatpak, and macOS packaging changes.
      applicationSource = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = nixpkgs.lib.fileset.unions [
          ./apps/gui-slint
          ./crates/native
          ./resources
          ./scripts/deconjugator.json
        ];
      };

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
        pkgs.libx11
        pkgs.libxcursor
        pkgs.libxi
        pkgs.libxcb
      ];

      linuxDevelopmentPackages = pkgs: [
        # Wayland ScreenCast portal / PipeWire support.
        pkgs.pipewire

        # Headers and pkg-config metadata used by the native UI and PipeWire.
        pkgs.fontconfig.dev
        pkgs.noto-fonts-cjk-sans
        pkgs.libxkbcommon.dev
        pkgs.wayland.dev
        pkgs.libx11.dev
        pkgs.libxcursor.dev
        pkgs.libxi.dev
        pkgs.libxcb.dev
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
            SDKROOT = "${pkgs.apple-sdk.sdkroot}";
            RUSTFLAGS = "-C link-arg=-L${unstablePkgs.swift}/lib/swift/macosx -C link-arg=-L${pkgs.apple-sdk.sdkroot}/usr/lib/swift";
            CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER =
              "${linker}/bin/meikipop-rust-linker";
          };
        };

      commonDevelopmentPackages =
        { pkgs, unstablePkgs }:
        [
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
          version = appVersion;
          src = applicationSource;

          cargoRoot = "apps/gui-slint";
          buildAndTestSubdir = "apps/gui-slint";
          cargoLock.lockFile = ./apps/gui-slint/Cargo.lock;

          dontUseSwiftpmBuild = true;
          dontUseSwiftpmCheck = true;

          nativeBuildInputs = [
            pkgs.pkg-config
            unstablePkgs.rustPlatform.bindgenHook
          ]
          ++ pkgs.lib.optionals isLinux [
            pkgs.addDriverRunpath
            pkgs.makeWrapper
          ]
          ++ pkgs.lib.optionals isDarwin darwinToolchain.packages;

          buildInputs = [
            pkgs.openssl
            onnxruntime
          ] ++ pkgs.lib.optionals isLinux runtimeLibraries;

          env = {
            ORT_LIB_LOCATION = "${onnxruntime}";
            ORT_PREFER_DYNAMIC_LINK = "1";
          } // pkgs.lib.optionalAttrs isDarwin darwinToolchain.environment;

          postFixup = pkgs.lib.optionalString isLinux ''
            addDriverRunpath "$out/bin/meikipop-gui"
            wrapProgram "$out/bin/meikipop-gui" \
              --prefix PATH : "${unstablePkgs.python3}/bin" \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath runtimeLibraries}"
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

      mkDarwinApp =
        system:
        let
          packageSets = packageSetsFor system;
          inherit (packageSets) pkgs;
          meikipop = mkPackage system;
          infoPlist = pkgs.writeText "MeikiPop-Info.plist" ''
            <?xml version="1.0" encoding="UTF-8"?>
            <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            <plist version="1.0">
            <dict>
              <key>CFBundleDevelopmentRegion</key><string>en</string>
              <key>CFBundleExecutable</key><string>MeikiPop</string>
              <key>CFBundleIconFile</key><string>MeikiPop</string>
              <key>CFBundleIdentifier</key><string>${bundleId}</string>
              <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
              <key>CFBundleName</key><string>${appName}</string>
              <key>CFBundlePackageType</key><string>APPL</string>
              <key>CFBundleShortVersionString</key><string>${appVersion}</string>
              <key>CFBundleVersion</key><string>${bundleVersion}</string>
              <key>LSMinimumSystemVersion</key><string>${minimumMacOSVersion}</string>
              <key>NSHighResolutionCapable</key><true/>
              <key>NSScreenCaptureUsageDescription</key>
              <string>MeikiPop captures the selected screen or window to recognize Japanese text.</string>
            </dict>
            </plist>
          '';
        in
        pkgs.stdenvNoCC.mkDerivation {
          pname = "meikipop-app";
          version = appVersion;

          dontUnpack = true;

          nativeBuildInputs = [
            pkgs.darwin.autoSignDarwinBinariesHook
            pkgs.darwin.sigtool
            pkgs.imagemagick
            pkgs.libicns
            pkgs.macdylibbundler
          ];

          installPhase = ''
            runHook preInstall

            app="$out/Applications/MeikiPop.app"
            mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"

            cp "${meikipop}/bin/meikipop-gui" "$app/Contents/MacOS/MeikiPop"
            chmod +x "$app/Contents/MacOS/MeikiPop"

            dylibbundler \
              -od \
              -b \
              -ns \
              -x "$app/Contents/MacOS/MeikiPop" \
              -d "$app/Contents/Frameworks" \
              -p '@executable_path/../Frameworks/'

            # Remove build-machine search paths after dylibbundler has changed
            # every non-system dependency to @executable_path.
            find "$app/Contents/MacOS" "$app/Contents/Frameworks" -type f -print0 \
              | while IFS= read -r -d $'\0' binary; do
                  otool -l "$binary" 2>/dev/null \
                    | awk '/cmd LC_RPATH/ { getline; getline; print $2 }' \
                    | grep '^/nix/store/' \
                    | while IFS= read -r rpath; do
                        install_name_tool -delete_rpath "$rpath" "$binary"
                      done || true
                done

            # Start with the largest frame. Frame zero is only 16x16 and looks
            # visibly blurred when expanded to Finder's larger icon sizes.
            for size in 16 32 48 128 256 512; do
              magick "${./resources/icon.ico}[8]" \
                -resize "''${size}x''${size}" "/tmp/meikipop-''${size}.png"
            done
            png2icns "$app/Contents/Resources/MeikiPop.icns" \
              /tmp/meikipop-{16,32,48,128,256,512}.png

            cp "${infoPlist}" "$app/Contents/Info.plist"

            # Fail the build if a newly introduced library escapes relocation.
            find "$app/Contents/MacOS" "$app/Contents/Frameworks" -type f -print0 \
              | while IFS= read -r -d $'\0' binary; do
                  if otool -L "$binary" 2>/dev/null \
                    | tail -n +2 \
                    | grep -q '/nix/store/'; then
                    echo "Unbundled Nix dependency in $binary:" >&2
                    otool -L "$binary" >&2
                    exit 1
                  fi
                done

            runHook postInstall
          '';

          meta = {
            description = "Portable unsigned MeikiPop macOS application bundle";
            platforms = [ "aarch64-darwin" ];
          };
        };

      mkDarwinDmgBuilder =
        system:
        let
          packageSets = packageSetsFor system;
          inherit (packageSets) pkgs;
          app = mkDarwinApp system;
        in
        pkgs.writeShellApplication {
          name = "build-meikipop-dmg";
          runtimeInputs = [ pkgs.coreutils ];
          text = ''
            if [ "$(uname -s)" != Darwin ]; then
              echo "The MeikiPop DMG can only be created on macOS." >&2
              exit 1
            fi

            mkdir -p dist
            work_dir="$(mktemp -d)"
            trap 'rm -rf "$work_dir"' EXIT
            payload_dir="$work_dir/payload"
            mkdir "$payload_dir"
            staged_app="$payload_dir/${appName}.app"
            staged_dmg="$work_dir/${appName}-aarch64.dmg"

            # Apple recommends ditto for copying bundles because it preserves
            # metadata, symlinks, resource forks, and extended attributes.
            /usr/bin/ditto "${app}/Applications/${appName}.app" "$staged_app"
            chmod -R u+w "$staged_app"

            # Relocated nested Mach-O files were ad-hoc signed by the Nix
            # derivation. Sign the outer bundle last to create its resource
            # seal. This uses no identity, certificate, or Apple account.
            /usr/bin/codesign \
              --force \
              --identifier "${bundleId}" \
              --sign - \
              "$staged_app"
            /usr/bin/codesign --verify --deep --strict "$staged_app"
            /usr/bin/plutil -lint "$staged_app/Contents/Info.plist" >/dev/null

            ln -s /Applications "$payload_dir/Applications"

            /usr/bin/hdiutil create \
              -volname "${appName}" \
              -srcfolder "$payload_dir" \
              -format UDZO \
              "$staged_dmg"

            /usr/bin/hdiutil verify "$staged_dmg" >/dev/null
            mv -f "$staged_dmg" "dist/${appName}-aarch64.dmg"

            echo "Created dist/${appName}-aarch64.dmg"
          '';
        };

      mkFlatpakTools =
        system:
        let
          packageSets = packageSetsFor system;
          inherit (packageSets) pkgs unstablePkgs;
          manifest = "flatpak/${flatpakAppId}.yml";

          updateSources = pkgs.writeShellApplication {
            name = "update-meikipop-flatpak-sources";
            runtimeInputs = [ unstablePkgs.flatpak-builder-tools ];
            text = ''
              if [ ! -f "${manifest}" ]; then
                echo "Run this command from the MeikiPop repository root." >&2
                exit 1
              fi

              flatpak-cargo-generator \
                apps/gui-slint/Cargo.lock \
                -o flatpak/cargo-sources.json
            '';
          };

          buildBundle = pkgs.writeShellApplication {
            name = "build-meikipop-flatpak";
            runtimeInputs = [
              pkgs.coreutils
              unstablePkgs.flatpak
              unstablePkgs.flatpak-builder
            ];
            text = ''
              if [ ! -f "${manifest}" ] || [ ! -f flatpak/cargo-sources.json ]; then
                echo "Run this command from the MeikiPop repository root." >&2
                echo "If Cargo sources are missing, run: nix run .#update-flatpak-sources" >&2
                exit 1
              fi

              for runtime in \
                "org.freedesktop.Platform//${flatpakRuntimeVersion}" \
                "org.freedesktop.Sdk//${flatpakRuntimeVersion}" \
                "org.freedesktop.Sdk.Extension.rust-stable//${flatpakRuntimeVersion}" \
                "org.freedesktop.Sdk.Extension.llvm22//${flatpakRuntimeVersion}"; do
                if ! flatpak info "$runtime" >/dev/null 2>&1; then
                  echo "Missing Flatpak runtime: $runtime" >&2
                  echo "Install it with: flatpak install flathub $runtime" >&2
                  exit 1
                fi
              done

              mkdir -p dist
              flatpak-builder \
                --force-clean \
                --repo=bundle-repo \
                build-flatpak \
                "${manifest}"
              flatpak build-bundle \
                --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
                bundle-repo \
                "dist/meikipop.flatpak" \
                "${flatpakAppId}"

              echo "Created dist/meikipop.flatpak"
            '';
          };
        in
        {
          inherit buildBundle updateSources;

          shell = pkgs.mkShell {
            packages = [
              unstablePkgs.appstream
              unstablePkgs.flatpak
              unstablePkgs.flatpak-builder
              unstablePkgs.flatpak-builder-tools
            ];
            shellHook = ''
              echo "MeikiPop Flatpak tools"
              echo "  Refresh Cargo sources: nix run .#update-flatpak-sources"
              echo "  Build bundle:          nix run .#build-flatpak"
            '';
          };
        };
    in
    {
      packages = forAllSystems (
        system:
        {
          default = mkPackage system;
        }
        // nixpkgs.lib.optionalAttrs (system == "aarch64-darwin") {
          macos-app = mkDarwinApp system;
        }
      );

      apps = forAllSystems (
        system:
        {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/meikipop-gui";
            meta.description = "Run MeikiPop";
          };
        }
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") (
          let
            tools = mkFlatpakTools system;
          in
          {
            build-flatpak = {
              type = "app";
              program = "${tools.buildBundle}/bin/build-meikipop-flatpak";
              meta.description = "Build a local MeikiPop Flatpak bundle";
            };
            update-flatpak-sources = {
              type = "app";
              program = "${tools.updateSources}/bin/update-meikipop-flatpak-sources";
              meta.description = "Refresh vendored Cargo sources for Flatpak";
            };
          }
        )
        // nixpkgs.lib.optionalAttrs (system == "aarch64-darwin") {
          build-dmg = {
            type = "app";
            program = "${mkDarwinDmgBuilder system}/bin/build-meikipop-dmg";
            meta.description = "Build a portable unsigned MeikiPop DMG";
          };
        }
      );

      devShells = forAllSystems (
        system:
        if system == "x86_64-linux" then
          {
            default = mkLinuxShell system false;
            cuda = mkLinuxShell system true;
            flatpak = (mkFlatpakTools system).shell;
          }
        else
          {
            default = mkDarwinShell system;
          }
      );
    };
}
