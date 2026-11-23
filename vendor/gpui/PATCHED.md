# Orchard's copy of gpui 0.2.2

Unchanged from crates.io except one line in Cargo.toml: `core-foundation`
"=0.10.0" relaxed to "0.10", so it resolves together with nusb (which needs
0.10.1 or later). 0.10.1 is a semver-compatible patch release.

Trimmed to what the build uses: the crate's examples, tests and docs are
removed, with their `[[example]]` and `[[test]]` entries in Cargo.toml.
