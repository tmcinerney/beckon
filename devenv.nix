{ lib, pkgs, ... }:
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
    nixfmt.enable = true;
  };

  # AIDEV-NOTE: devenv's stock install task runs `prek install -c "$DEVENV_ROOT/..."`
  # into Git's hooks dir. Linked worktrees share that dir, so the last worktree to
  # enter a shell baked its absolute config path into every worktree's hook, and
  # deleting it broke commits everywhere. The committed .githooks/pre-commit
  # resolves the config per worktree instead, so the task must not install.
  tasks."devenv:git-hooks:install".exec = lib.mkForce ''
    hooks_dir="$(git rev-parse --path-format=absolute --git-path hooks 2>/dev/null)" || exit 0
    if grep -qs 'hook-impl' "$hooks_dir/pre-commit" \
      && ! grep -qs 'beckon: worktree-relative hook' "$hooks_dir/pre-commit"; then
      echo 1>&2 "beckon: $hooks_dir/pre-commit is a prek-generated hook with a fixed config path."
      echo 1>&2 "beckon: run: git config --local core.hooksPath .githooks"
    fi
  '';
}
