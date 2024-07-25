{
  description = "Process multiplexer";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";
  };

  outputs = { self, flake-utils, nixpkgs }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ my-overlay ];
        };
        my-overlay = final: prev: { };
        package-name = "process-muxer";

      in
      {
        inherit pkgs;

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ cargo rust-analyzer rustfmt cargo-watch rustPackages.clippy ];
          inputsFrom = [ self.packages."${system}"."${package-name}" ];
        };
        packages = {
          default = self.packages."${system}"."${package-name}";
          # Read the docs at https://nixos.org/manual/nixpkgs/stable/#rust
          "${package-name}" = pkgs.rustPlatform.buildRustPackage {
            pname = package-name;
            version = "0.1.0";
            cargoSha256 = "sha256-+/awS6As4kr9xI/Ux6ksmDLx88qdHHG7WrktuFWK1BU=";
            src = ./src;
            buildInputs =
              let
                darwin-frameworks = with pkgs.darwin.apple_sdk.frameworks;
                  [
                    CoreFoundation
                    CoreServices
                    SystemConfiguration
                    pkgs.libiconv
                  ];
                system-dependent = if pkgs.stdenv.isDarwin then darwin-frameworks else [ ];
              in
              system-dependent;
          };
        };
      });
}
