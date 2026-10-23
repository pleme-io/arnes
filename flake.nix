{
  description = "Rust/tatara-lisp open-source Claude-Code-equivalent agent harness";

  # substrate.rust.workspace dispatches over Cargo.gen.lock (the slim gen delta,
  # reconstructed to the full BuildSpec in pure Nix) — no crate2nix, no Cargo.nix.
  inputs.substrate.url = "github:pleme-io/substrate";
  # blue's project engine lowers ./Bluefile: arnes's bidamas (docs/kernel.md).
  inputs.blue.url = "github:pleme-io/blue";

  outputs = { substrate, blue, ... }:
    let
      base = substrate.rust.workspace {
        src = ./.;
        member = "arnes";
      };
      # The bidamas' packages, checks (tests, collisions, locks, catalogue) and
      # `regen`, beside the Rust outputs. Only names the engine does not share
      # with the Rust flake are taken, so `default` stays the arnes binary.
      bidamas = blue.lib.project { src = ./.; };
      gated = base // {
        packages = builtins.mapAttrs
          (system: own: own // {
            default = own.default.overrideAttrs {
              kernelRatchet = bidamas.checks.${system}.generated-kernel-ratchet-fresh;
            };
          })
          base.packages;
      };
      merge = attr: builtins.mapAttrs
        (system: own: own // builtins.removeAttrs (bidamas.${attr}.${system} or { }) [ "default" ])
        (gated.${attr} or { });
    in
    gated // {
      packages = merge "packages";
      checks = merge "checks";
      apps = merge "apps";
      # Re-attached after the substrate.rust.workspace migration dropped it.
      # ./module is an HM-only module ({ self }: { config, lib, pkgs, ... }: …)
      # that reads self.packages.${system}.default — the base flake provides it.
      homeManagerModules.default = import ./module { self = gated; };
    };
}
