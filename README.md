# Orchard tools

The programs that run on your computer to put Linux on an iPad or iPhone and
start it, built in Rust on shared libraries:

- **The installer**: a graphical app that splits the device's storage, installs
  a distribution such as [Omarchy A](https://github.com/OrchardLinux/omarchy-a),
  and sets up the launcher. One generic build; each distribution stamps it with
  its name, wordmark and release.
- **Orchard**: the launcher, which starts an installed system. It is neutral
  and shared by every distribution.
- **The command line** (`orchard2`, in progress): the same building blocks for
  scripts and development.

They build on [orchard](https://github.com/OrchardLinux/orchard), which holds
what runs on the device. macOS (Apple Silicon and Intel) and Linux (x86_64 and
aarch64).

## Layout

| Path | What it is |
| --- | --- |
| `crates/app` | The installer and Orchard windows (GPUI) |
| `crates/flow` | Their state: one statechart, one update function, no I/O |
| `crates/runtime` | Runs the flow: downloads, boots, the companion link (usbmux), the install |
| `crates/usb` | USB transports: DFU, recovery, Pongo, m1n1 |
| `crates/boards` | Device descriptions and firmware recipes |
| `crates/formats` | File formats Apple devices and Linux boots use |
| `crates/cache` | Pinned downloads and the cache |
| `crates/build` | `build`: the component cache and orchard's producers |
| `crates/device`, `crates/cli` | The command line ([`crates/cli/README.md`](crates/cli/README.md)) |
| `companion` | The iPad app that splits the storage, with its helper |
| `packaging` | Release builds, stamping (`stamp.py`) and `install.sh` |
| `vendor` | Patched or pinned third-party code (see each `PATCHED.md`) |

## Build

```sh
cargo build --release -p orchard-installer-app   # orchard-installer and orchard
cargo test --workspace
```

Linux needs libusb, libxkbcommon and the Vulkan and Wayland/X11 headers; the
companion needs Xcode (`companion/build.sh`). Release artifacts:
`packaging/build-macos.sh`, `packaging/build-linux.sh <arch>` (Podman) and
`packaging/package-linux.sh <arch>`, then `packaging/stamp.py` per distribution.

## License

GPL-2.0-or-later (`LICENSE`). Vendored code keeps its own license, in its
directory.
