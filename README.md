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
