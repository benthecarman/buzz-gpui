{
  description = "Buzz GPUI client and agent host daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, crane, ... }:
    {
      nixosModules.default = import ./nix/module.nix self;
    }
    // flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        runtimeLibs = with pkgs; [
          wayland
          vulkan-loader
          libxcb
          libxkbcommon
          libx11
          libxcursor
          libxi
          fontconfig
          freetype
          openssl
          dbus
          sqlite
        ];
        # Audio and video playback. GStreamer's setup hook puts each plugin
        # set on GST_PLUGIN_SYSTEM_PATH_1_0 inside the shell.
        playback = with pkgs.gst_all_1; [
          gstreamer
          gst-plugins-base
          gst-plugins-good
          gst-libav
        ];
        craneLib = crane.mkLib pkgs;
        # buzz-agent-controller reads two files from outside its crate
        # directory, which cargo vendoring leaves behind; take them from the
        # same pinned fork commit. Both fetches match the ones crane makes
        # for Cargo.lock, so each repository is cloned once.
        buzzApp = builtins.fetchGit {
          url = "https://github.com/benthecarman/buzz-app";
          rev = "ec7d795f2da780c93f927e7f8ae18eda921155cf";
          allRefs = true;
          submodules = true;
        };
        # buzz-agent likewise reads its model manifest from the buzz
        # workspace's scripts directory.
        buzz = builtins.fetchGit {
          url = "https://github.com/block/buzz.git";
          rev = "4f51b9e1010e086a16c099cd8d8218ca974a5e18";
          allRefs = true;
          submodules = true;
        };
        buzzdArgs = {
          pname = "buzzd";
          version = "0.1.0";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              let base = baseNameOf path; in
              !(builtins.elem base [ "target" "target-deb" "dist" ".test-relay" ".claude" "docs" ]);
          };
          cargoExtraArgs = "--locked -p buzzd";
          # Tests need a display, a keyring or a relay; they run in CI-like
          # environments, not in the package build.
          doCheck = false;
          nativeBuildInputs = with pkgs; [ pkg-config cmake perl ];
          buildInputs = with pkgs; [ dbus openssl ];
          # The vendored crates sit in the read-only store. Copy the git
          # sources of those two crates into the build directory and put the
          # files where they look. Store timestamps keep the dependency build
          # fresh when the package build reuses it.
          postConfigure = ''
            vendor="$NIX_BUILD_TOP/vendor"
            mkdir -p "$vendor/runtime" "$vendor/scripts"
            for dir in $(sed -n 's/^directory = "\(.*\)"$/\1/p' "$CARGO_HOME/config.toml"); do
              if ls -d "$dir"/buzz-agent-* >/dev/null 2>&1; then
                copy="$vendor/$(basename "$dir")"
                cp -rL --preserve=timestamps "$dir" "$copy"
                chmod -R u+w "$copy"
                cp --preserve=timestamps ${buzzApp}/crates/build_env.rs "$copy/"
                substituteInPlace "$CARGO_HOME/config.toml" --replace-fail "\"$dir\"" "\"$copy\""
              fi
            done
            cp --preserve=timestamps ${buzzApp}/runtime/agent-runtime.json "$vendor/runtime/"
            cp --preserve=timestamps ${buzz}/scripts/model-capabilities.json "$vendor/scripts/"
            # buzz-agent-controller's build script watches an optional
            # .env.local, and a missing file reruns it on every build. An
            # empty one sets nothing, the same as none.
            touch -d @1 "$vendor/.env.local"
          '';
        };
      in
      {
        # The dependencies build in their own derivation, so CI can keep
        # them and rebuild only this workspace's crates.
        packages.buzzd = craneLib.buildPackage (buzzdArgs // {
          cargoArtifacts = craneLib.buildDepsOnly (buzzdArgs // {
            # Only the release build is reused; skip crane's extra check.
            buildPhaseCargoCommand = "cargoWithProfile build --locked -p buzzd";
          });
          meta.mainProgram = "buzzd";
        });
        packages.default = self.packages.${system}.buzzd;

        devShells.default = pkgs.mkShell {
          # The Rust toolchain buzzd is built with, so local and CI checks match.
          nativeBuildInputs = with pkgs; [ pkg-config cmake perl rustc cargo clippy rustfmt ];
          buildInputs = runtimeLibs ++ playback ++ (with pkgs; [ vulkan-headers alsa-lib zstd ]);
          env = {
            RUST_BACKTRACE = "1";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          };
        };
      });
}
