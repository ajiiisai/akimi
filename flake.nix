{
  description = "Akimi - fast ext4 disk usage scanner";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [
              cargo
              clang
              clippy
              cmake
              hyperfine
              pkg-config
              rustc
              rustfmt
            ];

            buildInputs = with pkgs; [
              alsa-lib
              e2fsprogs
              fontconfig
              freetype
              libxkbcommon
              libx11
              libxcb
              vulkan-loader
              wayland
            ];

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [
              alsa-lib
              libxkbcommon
              libx11
              libxcb
              vulkan-loader
              wayland
            ]);
          };
        });
    };
}
