# cargo-anywhere

Offload Cargo builds to a fast remote machine over SSH and `rsync`, with automatic client CPU optimization, workspace/`../` path-dependency support, and opt-in Nix / `devenv` integration.

## Features

- **Workspace & `../` path dependencies**: Uses `cargo metadata` to find the common ancestor of your workspace and local `path` dependencies, preserving relative paths on the remote host.
- **Automatic client CPU optimization**: Detects your local CPU (e.g., `znver4`) and passes `-C target-cpu=<cpu>` via `RUSTFLAGS` to the remote build host (automatically skipped when `--target` is passed).
- **Smart `run` & binary-only copy-back**: `cargo anywhere run` builds remotely, copies back **only** the compiled executable + `Cargo.lock` (never gigabytes of `target/`), and executes locally with your `-- <app_args>`.
- **Opt-in Nix / `devenv` support**: Plain `cargo` by default for standard `rustup` users; set `env = "auto" | "devenv" | "nix"` to wrap remote commands in `devenv shell --` or `nix develop -c`.
- **Isolated build user & SSH key**: Optional `user` and `identity_file` (`--user`, `--identity-file`) to separate build connections from your normal SSH user/key.
- **Remote cache cleanup**: `cargo anywhere gc` prunes remote build directories.

## Requirements

- **Client**: `ssh`, `rsync`, `cargo`
- **Remote host**: `ssh`, `rsync`, `cargo` (plus `devenv` / `nix` if using `env = "devenv" | "nix" | "auto"`)

## Installation

### Nix Flake

Add `cargo-anywhere` to your flake inputs and install `cargo-anywhere.packages.${system}.default`:

```nix
inputs.cargo-anywhere.url = "github:Rudi-Regentonne/cargo-anywhere";
```

### From Source

```sh
cargo install --path .
```

## Usage

```sh
# Build, check, test, or clippy on the remote host (all non-`run` commands execute remotely)
cargo anywhere build --release
cargo anywhere check
cargo anywhere test
cargo anywhere clippy -- -D warnings

# Cross-compile (skips automatic host target-cpu)
cargo anywhere build --target wasm32-unknown-unknown

# Build remotely, copy back only the binary, and run locally (default for `run`)
cargo anywhere run --release -- --port 8080

# Force remote execution for `run`, or force local execution
cargo anywhere --remote-run run
cargo anywhere --local-run run

# Copy back full target/ directory instead of just binaries
cargo anywhere --copy-target build

# Use a separate build user and SSH key
cargo anywhere --user builder --identity-file ~/.ssh/id_builder build

# Clean remote build cache
cargo anywhere gc
```

## Configuration

Precedence (highest to lowest):

1. **CLI flags**: `--host`, `--user`, `--identity-file`, `--remote-dir`, `--env`, `--target-cpu`, `--no-target-cpu`, `--copy-bin`, `--copy-target`, `--no-copy`, `--remote-run`, `--local-run`
2. **Environment variables**: `CARGO_ANYWHERE_HOST`, `CARGO_ANYWHERE_USER`, `CARGO_ANYWHERE_IDENTITY_FILE`, `CARGO_ANYWHERE_REMOTE_DIR`, `CARGO_ANYWHERE_ENV`, `CARGO_ANYWHERE_TARGET_CPU`, `CARGO_ANYWHERE_COPY`, `CARGO_ANYWHERE_REMOTE_RUN`
3. **Workspace local override**: `<workspace>/.cargo-anywhere.local.toml`
4. **Workspace config**: `<workspace>/.cargo-anywhere.toml`
5. **Global config**: `~/.config/cargo-anywhere/config.toml`

Example `~/.config/cargo-anywhere/config.toml`:

```toml
host = "heinz"
# user = "builder"
# identity_file = "~/.ssh/id_builder"
remote_dir = "~/remote-builds"
env = "auto"          # none (default) | devenv | nix | auto
target_cpu = "auto"   # auto (default) | <cpu> | none
# copy = "bin"        # none | bin | target
# remote_run = false
```

---

## Inspired by

[sgeisler/cargo-remote](https://github.com/sgeisler/cargo-remote)
