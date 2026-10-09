{
  description = "Buzz GPUI client and agent host daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
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
      in
      {
        packages.buzzd = pkgs.rustPlatform.buildRustPackage {
          pname = "buzzd";
          version = "0.1.0";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              let base = baseNameOf path; in
              !(builtins.elem base [ "target" "target-deb" "dist" ".test-relay" ".claude" "docs" ]);
          };
          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
          };
          cargoBuildFlags = [ "-p" "buzzd" ];
          # buzz-agent-controller reads two files from outside its crate
          # directory, which cargo vendoring leaves behind. Put them where the
          # vendored crate looks, from the same pinned fork commit.
          preBuild =
            let
              buzzApp = builtins.fetchGit {
                url = "https://github.com/benthecarman/buzz-app";
                rev = "6958ff1fc2bf1dc682a9e445acdaaa020fb17a58";
                allRefs = true;
              };
            in
            ''
              cp ${buzzApp}/crates/build_env.rs "$NIX_BUILD_TOP/cargo-vendor-dir/build_env.rs"
              mkdir -p "$NIX_BUILD_TOP/runtime"
              cp ${buzzApp}/runtime/agent-runtime.json "$NIX_BUILD_TOP/runtime/"
            '';
          # Tests need a display, a keyring or a relay; they run in CI-like
          # environments, not in the package build.
          doCheck = false;
          nativeBuildInputs = with pkgs; [ pkg-config cmake perl ];
          buildInputs = with pkgs; [ dbus openssl ];
          meta.mainProgram = "buzzd";
        };
        packages.default = self.packages.${system}.buzzd;

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ pkg-config cmake perl ];
          buildInputs = runtimeLibs ++ playback ++ (with pkgs; [ vulkan-headers alsa-lib zstd ]);
          env = {
            RUST_BACKTRACE = "1";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          };
        };
      });
}
