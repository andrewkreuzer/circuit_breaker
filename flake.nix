{
  description = "A circuit breaker implementation in Rust";

  inputs = {
    nixpkgs.url      = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url  = "github:numtide/flake-utils";
    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs = { nixpkgs, rust-overlay, flake-utils, treefmt-nix, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustPlatform = pkgs.makeRustPlatform {
          cargo = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default);
          rustc = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default);
        };
      in
      {
        packages.default = rustPlatform.buildRustPackage {
          pname = "trip";
          version = (builtins.fromTOML
            (builtins.readFile ./Cargo.toml)).package.version;

          src = ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          buildFeatures = [];

          nativeBuildInputs = with pkgs; [
            openssl
            pkg-config
          ];

          buildInputs = with pkgs; [
            openssl
            pkg-config
          ];

          meta = {
            description = "Trip a breaker when functions fail";
            homepage = "https://github.com/andrewkreuzer/trip";
            license = with pkgs.lib.licenses; [ mit unlicense ];
            maintainers = [{
              name = "Andrew Kreuzer";
              email = "me@andrewkreuzer.com";
              github = "andrewkreuzer";
              githubId = 17596952;
            }];
          };
        };

        imports = [
          treefmt-nix.flakeModule
        ];
        treefmt.config = {
          projectRootFile = "flake.nix";
          programs.nixpkgs-fmt.enable = true;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            openssl
            pkg-config
            rust-bin.nightly.latest.default
            rust-analyzer
            nixd
          ];
        };
      }
    );
}
