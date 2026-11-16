{
  description = "ppk2_tool — Nordic Power Profiler Kit II driver, daemon and GUI";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };
  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (pkgsFor system));
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
      guiLibs =
        pkgs: with pkgs; [
          libGL
          libxkbcommon
          vulkan-loader
          wayland
          xorg.libX11
          xorg.libXcursor
          xorg.libXi
          xorg.libXrandr
        ];
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
    in
    {
      packages = forAllSystems (pkgs: {
        default = self.packages.${pkgs.system}.ppk2_tool;
        ppk2_tool = pkgs.rustPlatform.buildRustPackage {
          pname = "ppk2_tool";
          inherit version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          postInstall = ''
            install -Dm644 ${./packaging/70-ppk2.rules} $out/lib/udev/rules.d/70-ppk2.rules
            install -Dm644 ${./packaging/ppk2_tool.desktop} $out/share/applications/ppk2_tool.desktop
          '';
          postFixup = ''
            patchelf --add-rpath ${pkgs.lib.makeLibraryPath (guiLibs pkgs)} $out/bin/ppk2_tool-gui
          '';
          meta = {
            description = "Nordic Power Profiler Kit II without nRF Connect: daemon, CLI recorder, egui GUI";
            homepage = "https://github.com/slaxor/ppk2_tool";
            license = pkgs.lib.licenses.gpl3Only;
            mainProgram = "ppk2_tool-gui";
            platforms = systems;
          };
        };
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.ppk2_tool}/bin/ppk2_tool-gui";
        };
        cli = {
          type = "app";
          program = "${self.packages.${pkgs.system}.ppk2_tool}/bin/ppk2_tool";
        };
      });

      overlays.default = final: prev: { ppk2_tool = self.packages.${final.system}.ppk2_tool; };

      # programs.ppk2-tool.enable = true;  (+ optional settings = { fold = 100; })
      homeManagerModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.ppk2-tool;
          tomlFormat = pkgs.formats.toml { };
        in
        {
          options.programs.ppk2-tool = {
            enable = lib.mkEnableOption "ppk2_tool, host software for the Nordic Power Profiler Kit II";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.ppk2_tool;
              description = "The ppk2_tool package to install.";
            };
            settings = lib.mkOption {
              type = tomlFormat.type;
              default = { };
              example = {
                mode = "source";
                millivolts = 3300;
                fold = 10;
              };
              description = ''
                Starting settings, written to ~/.config/ppk2_tool/defaults.toml. Once the GUI
                or `ppk2_tool calibrate-voltage` has saved ~/.config/ppk2_tool/settings.toml,
                that file wins.
              '';
            };
          };
          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];
            xdg.configFile."ppk2_tool/defaults.toml" = lib.mkIf (cfg.settings != { }) {
              source = tomlFormat.generate "ppk2_tool-settings.toml" cfg.settings;
            };
          };
        };

      # hardware.ppk2.enable = true;  — udev rules: dialout access, no USB autosuspend
      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.hardware.ppk2;
        in
        {
          options.hardware.ppk2.enable = lib.mkEnableOption "udev rules for the Nordic Power Profiler Kit II";
          config = lib.mkIf cfg.enable {
            services.udev.packages = [ self.packages.${pkgs.system}.ppk2_tool ];
          };
        };

      devShells = forAllSystems (
        pkgs:
        let
          rust = pkgs.rust-bin.stable.latest.default.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
          };
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.just
              pkgs.cargo-nextest
            ]
            ++ guiLibs pkgs;
            LD_LIBRARY_PATH = "/run/opengl-driver/lib:" + pkgs.lib.makeLibraryPath (guiLibs pkgs);
          };
        }
      );

      checks = forAllSystems (pkgs: {
        build = self.packages.${pkgs.system}.ppk2_tool;
      });
    };
}
