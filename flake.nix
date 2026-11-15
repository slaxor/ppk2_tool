{
  description = "ppk2_tool";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };
  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      pkgs = import nixpkgs {
        system = "x86_64-linux";
        overlays = [ rust-overlay.overlays.default ];
      };
      rust = pkgs.rust-bin.stable.latest.default.override {
        extensions = [
          "rust-src"
          "rust-analyzer"
        ];
      };
      guiLibs = with pkgs; [
        libGL
        libxkbcommon
        vulkan-loader
        wayland
        xorg.libX11
        xorg.libXcursor
        xorg.libXi
        xorg.libXrandr
      ];
    in
    {
      devShells.x86_64-linux.default = pkgs.mkShell {
        packages = [
          rust
          pkgs.just
          pkgs.cargo-nextest
        ]
        ++ guiLibs;
        LD_LIBRARY_PATH = "/run/opengl-driver/lib:" + pkgs.lib.makeLibraryPath guiLibs;
      };
    };
}
