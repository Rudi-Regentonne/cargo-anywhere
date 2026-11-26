{ pkgs, ... }:

{
  packages = with pkgs; [
    rsync
  ];

  languages.rust.enable = true;

  git-hooks.hooks = {
    rustfmt.enable = true;

    clippy = {
      enable = true;
      entry = "cargo clippy --all-targets -- -D warnings";
      pass_filenames = false;
    };

    cargo-test = {
      enable = true;
      name = "cargo-test";
      entry = "cargo test";
      pass_filenames = false;
    };
  };
}
