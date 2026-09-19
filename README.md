# earth-files
A fork meant to be used as standalone file manager without depending on desktop environment
and theme/settings daemons.
I love what cosmic does, but it's not ready yet(at least for me) and until then I'll maintain that fork.
This fork is mostly written with AI as I don't have time to commit to a temp solution that I just want to work.

## Build the project from source

```sh
# Clone the project using `git`
git clone https://github.com/owl-m/earth-files
# Change to the directory that was created by `git`
cd earth-files
# Build an optimized version using `cargo`, this may take a while
cargo build --release
# Run the optimized version using `cargo`
cargo run --release
# Or run the built binary directly
./target/release/earth-files
```

Settings are stored in `$XDG_CONFIG_HOME/earth-files/` (normally
`~/.config/earth-files/`). To reuse settings from this fork's previous name, copy
the RON files from `~/.config/cosmic-files/` into that directory.

## License

This project is licensed under [GPLv3](LICENSE)

## Nix

The flake supports `x86_64-linux` and `aarch64-linux`:

```sh
nix develop              # Rust tools and native build dependencies
cargo build              # Build inside the development shell
nix build                # Build the release package in ./result
nix run                  # Launch the packaged app
nix run . -- ~/Downloads # Open a directory
nix flake check          # Build the package and run its library tests
nix fmt                  # Format the Nix files
```

The package installs the desktop entry, AppStream metadata and icons. Its wrapper
supplies runtime libraries, icon and MIME data, and `xdg-utils`. The shell shares
the package's build dependencies and adds Cargo, Clippy, rustfmt, rust-analyzer
and Just. Binaries built in the shell retain paths to the Wayland and Vulkan
libraries so they can also run after leaving it.

To install through Nix, add the flake as an input:

```nix
inputs.earth-files.url = "github:owl-m/earth-files";
inputs.earth-files.inputs.nixpkgs.follows = "nixpkgs";
```

The flake exports `overlays.default` for `pkgs.earth-files`, and
`packages.<system>.earth-files` for direct use.
