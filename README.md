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

## Theming

Colours live in two optional files:

```
~/.config/earth-files/themes/dark.ron
~/.config/earth-files/themes/light.ron
```

Both are read once at startup and each overrides the matching built-in
palette, so the file name decides what it starts from. A file that is absent
leaves its palette untouched, which is not an error, and one that cannot be
parsed is reported in the log and then ignored, never moved or rewritten.
Every field is optional.

```ron
(
    name: "gruvbox-dark",

    // A single colour sets a widget's base and the rest of its states are
    // worked out from it: hover, pressed, focus, selected, disabled, border
    // and the text drawn on top.
    accent: "#d65d0e",
    destructive: "#cc241d",
    success: "#98971a",
    warning: "#d79921",

    // Or name the states yourself; anything left out is still worked out
    // from the base.
    // accent: (base: "#d65d0e", hover: "#e07b30"),

    bg_color: "#282828",              // the window's own surface
    primary_container_bg: "#3c3836",  // panels and sidebars
    secondary_container_bg: "#504945",// controls inside a panel
    neutral_tint: "#3c3836",          // colours the grey ramp, keeping its shape
    text_tint: "#bdae93",             // text across the interface
    accent_text: "#fe8019",
    shade: "#00000080",

    corner_radii: (all: 8.0),         // or (radius_s: 4.0, radius_m: 8.0)
    // Applied on top of the configured density, so changing the density
    // still moves every step a theme does not pin.
    spacing: (space_m: 20),           // or a preset: Compact

    // The outline the window draws around itself, as a corner radius.
    // Left out, it draws none: square, no border, no inset, opaque, so the
    // compositor's own rounding and border are the only ones. Set it on a
    // compositor that decorates nothing; 0.0 gives a square outline.
    // window_outline: 12.0,

    // A standard button normally inherits the text colour of whatever it
    // sits on. Giving it a fill here makes it paint its own text instead.
    button: "#504945",
    list_button: "#3c3836",
    is_high_contrast: false,

    // Individual steps of the grey ramp, if the tint is not enough
    palette: (neutral_5: "#7c6f64", neutral_8: "#a89984"),
)
```

Colours are `#rgb`, `#rrggbb` or `#rrggbbaa`. A colour that is partly or
wholly transparent is judged against what shows through it when its text
colour is worked out: surfaces against the window, controls against the
panel they sit on. A key this version does not
recognise is logged and ignored, so a file written for a later version still
loads.
