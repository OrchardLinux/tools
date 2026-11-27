# Orchard tools

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/OrchardLinux/tools/main/install.sh | sh
```

puts `orchard`, the command line, into `~/.local/bin`: the newest build of
this repository's main branch that passed its tests, checked against its
checksums. `orchard update` replaces it with a newer one later. Then, with
an iPad or iPhone in DFU mode:

```sh
orchard install omarchy-a     # install a distribution
orchard launch                # start what is installed
orchard doctor                # what this computer has and lacks
```

[`docs/`](docs/README.md) has every command. The graphical installer comes
with each distribution's release.

To work on it, build it instead (Rust 1.98):

```sh
git clone https://github.com/OrchardLinux/tools && cd tools
cargo build --release -p orchard-cli    # target/release/orchard
```

## What is here

The programs that run on your computer to put Linux on an iPad or iPhone and
start it, built in Rust on shared libraries:

- **The installer**: a graphical app that splits the device's storage, installs
  a distribution such as [Omarchy A](https://github.com/OrchardLinux/omarchy-a),
  and sets up the launcher. One generic build; each distribution stamps it with
  its name, wordmark and release.
- **Orchard**: the launcher, which starts an installed system. It is neutral
  and shared by every distribution.
- **The command line** (`orchard`): the same building blocks for
  scripts and development. Its documentation is in [`docs/`](docs/README.md).

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
| `crates/remote` | `shell`, `exec`, `push`, `pull`: a running system over SSH or the serial console |
| `crates/device`, `crates/cli` | The command line |
| `docs` | What `orchard` does, command by command |
| `TODO.md` | What is decided but not done |
| `companion` | The iPad app that splits the storage, with its helper |
| `crates/inapp`, `ios-installer` | The installer that runs on the device itself: its engine, app and helper ([`ios-installer/README.md`](ios-installer/README.md)) |
| `packaging` | Release builds, stamping (`stamp.py`) and `install.sh` |
| `vendor` | Patched or pinned third-party code (see each `PATCHED.md`) |

## Build

```sh
cargo build --release -p orchard-installer-app   # orchard-installer and orchard-launcher
cargo test --workspace
```

Linux needs libxkbcommon and the Wayland, X11 (xcb), Vulkan, fontconfig,
FreeType, ALSA and OpenSSL headers for the windows
(`packaging/linux/Containerfile` lists the packages); the command line alone
needs none of them, and nothing needs libusb. The companion needs Xcode (`companion/build.sh`). Release artifacts:
`packaging/build-macos.sh`, `packaging/build-linux.sh <arch>` (Podman) and
`packaging/package-linux.sh <arch>`, then `packaging/stamp.py` per distribution.

## License

GPL-2.0-or-later (`LICENSE`). Vendored code keeps its own license, in its
directory.
