{ pkgs, ... }:
{
  languages.rust = {
    enable = true;
    channel = "stable";
  };

  packages = with pkgs; [
    clippy
    rust-analyzer
  ];

  git-hooks.hooks = {
    clippy.enable = true;
    rustfmt.enable = true;
    nixfmt-rfc-style.enable = true;
  };
}
