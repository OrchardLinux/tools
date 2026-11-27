# build

```sh
orchard2 build [DIRECTORY]
```

builds a project: a RAM payload, or in a distribution checkout its release.
It touches no device. It decides what is needed, looks each part up in the
cache, and has orchard's recipes make what is missing; it assembles nothing
itself.

## Two kinds of project

`orchard.toml` in the directory says which:

```toml
name = "minimal-boot"      # a RAM payload: init and rootfs/ beside this file
[payload]
transport = false          # leave the USB session helper out (default: in)
modules = true             # stage the kernel's modules (default: no)
firmware = true            # the board's firmware is added when it is run
```

```toml
name = "minimal-linux"     # a distribution: its own script builds the release
[release]
build = "tools/build.py"   # relative to this file
selects-platform = true    # the script takes --platform
```

A directory without `orchard.toml` is a payload with the defaults. One with
only the older `payload.toml` or `distro.toml` is refused.

`orchard.local.toml`, in the directory or above it up to the repository's
root, holds a developer's own choices and is never committed:

```toml
device = "j71b"            # the board to build for
platform = "apple-16k"     # or the platform
kernel-tree = "../linux"   # a kernel tree to build instead of orchard's
packages = "../packages"   # distributions: a checkout of OrchardLinux/packages; false: none
```

## A RAM payload

`DIRECTORY/init` and, if there, `DIRECTORY/rootfs/` become an initramfs,
which is packed with the kernel, every board's device tree of one platform
and the loader into one image.

- **Which platform**: `--platform`, or `--device BOARD` (its platform), or
  `orchard.local.toml`, or the board attached in DFU or Pongo mode.
- **Which board's modules**: a payload carries one board's external
  modules: the chosen board's, else the platform's first.
- **Result**, in `DIRECTORY/out/PLATFORM/`:

| File | Is |
| --- | --- |
| `linux.bin` | the image to load |
| `pongo.bin` | the Pongo to load it through (not for boards Cascadia starts) |
| `initramfs.cpio.gz` | the RAM filesystem alone |
| `build.json`, `linux.bin.json` | what the build and the image were made from |
| `commands.sh` | what the build ran, as plain commands |

`--explain` prints `commands.sh` instead of the result. Every line runs by
path with the variables the build gave it, without orchard2.

Parts given finished bypass their recipe: `--kernel-dir DIR` (a staged
kernel, device trees and modules), `--busybox FILE`, `--loader FILE` (m1n1,
or a board's Cascadia loader), `--pongo FILE`.

## A distribution's release

In a checkout with `[release]`, `build` runs the distribution's own script:

```
BUILD --version VERSION --output out/releases/VERSION [--platform P …]
```

with `ORCHARD_CHECKOUT` (the orchard data), `ORCHARD_TOOL` (this orchard2),
`ORCHARD_PYTHON`, `ORCHARD_CACHE`, and, when given, `ORCHARD_KERNEL_TREE`
and `ORCHARD_PACKAGES`. `--version` names the release (default
`dev-DATE-TIME`); `--platform` is passed on when the script selects
platforms. Afterwards the release's index and manifest are checked against
the files they pin, and `out/releases/latest.json` names it, which is what
`orchard2 install` without a source installs.

The script asks orchard2 for what orchard supplies finished:

```sh
orchard2 inputs --output FILE [--platform P …]
```

writes one JSON document: per platform its kernel bundle and its boards
with their device trees, the shared m1n1, Pongo and static tools, the
revision of orchard's data, and under `produced` how each part was got.
With 32-bit platforms the document is that root's (`"architecture": "arm"`):
ARMv7 tools and per platform each board's Cascadia loader. One document is
one architecture. `--kernel-dir P=DIR`, `--m1n1`, `--pongo`, `--tools DIR`,
`--loader BOARD=FILE` give a part finished; `--kernel-tree DIR` builds the
kernels from a tree.

`--packages DIR` (or `packages` in `orchard.local.toml`) is a checkout of
OrchardLinux/packages: the image's `[orchard]` packages are built from it
where it differs from the published repository.

## Recipes

The recipes (`kernel/build`, `boot/m1n1/build`, the assembly scripts in
`producers/` …) come from `--data DIR`, an orchard checkout, else from a
copy of orchard's newest nightly in the cache (`recipes/TAG`), fetched once
and refreshed by `orchard2 data update`. They are Python scripts and need
Python 3.11 or newer: `ORCHARD_PYTHON`, else `python3`, else the newest
`python3.N` on PATH. Most build in Podman containers; a failed recipe says
when Podman is missing, does not answer, or its VM is not running.

`producers/contract.toml` in the data says which files decide each
component and which version the kernel producers have. Data without it, or
that states more than this orchard2 knows, is refused.

## Components and their keys

Each part is a directory `components/NAME/KEY` in the cache, never changed
after it is made. The key is the SHA-256 of what decides its content: the
recipe files, the source tree, the build settings. Every file of an entry is
recorded with its hash and checked before each use; identical files are
stored once.

- **A hit** reuses the entry. **A miss** says which inputs differ from the
  newest entry, and builds.
- **Nightly components**: orchard's CI publishes kernels, m1n1 and the
  static tools under the same keys. A build takes the nightly's m1n1 and
  tools when its own key matches, and `inputs` also the kernel bundle;
  a payload's kernel is always built here. `ORCHARD_PUBLISHED=off` builds
  everything here; `ORCHARD_KERNEL=local` only the kernels.
- `orchard2 component-keys --output FILE [PLATFORM…]` writes those keys
  as CI publishes them (`component-keys.json`), from the data's files
  alone, for comparing with what CI published. CI itself does not use
  orchard2. It is not listed in `--help`.
- **Pongo** needs macOS with Xcode to build and is the same for every
  device: an unchanged recipe uses the released binary.

## The kernel

One per platform, in two layers: the base (the compiled kernel, every
board's device tree, the in-tree modules, and what external modules build
against) and a board's layer (its external modules).

- **Orchard's own source** is OrchardLinux/linux at the commit the
  platform's lock names, kept as an editable tree in the cache
  (`kernel-work/PLATFORM/source`) beside the compiler's output. Its
  identity is the Git tree its content would be committed as, so local
  edits are part of the key and build incrementally.
- **Another commit in the lock** moves that one tree in place: only the
  files that differ are written, so only those compile again. With local
  edits in the tree the move is refused, never discarded. Two checkouts
  whose locks name different commits share the tree and move it back and
  forth; `--kernel-tree` is for work that should keep its own.
- **`--kernel-tree DIR`** builds a developer's tree as it is, inside itself
  (`DIR/.orchard-out/`), cached apart.

## Space

A build refuses to start below 10 GB free (`ORCHARD_MIN_FREE_GB`). Before
it starts it deletes least recently used entries until the build cache fits
its limit of 20 GiB (`ORCHARD_CACHE_BUDGET_GB`): older components, work
directories and kernel trees without local edits, never the newest of each,
and what interrupted builds left.

## Several builds, and Ctrl-C

Builds share the cache; one process makes an entry while others wait for
it, and a kernel tree has its own lock. Ctrl-C ends the running recipe and
the build removes what it staged; a second one ends it at once. The exit
status is 130.

Producers print to stderr; stdout carries the result, or with `--explain`
the script. A failed recipe gives the error `build` (exit status 15).
Variables: [reference.md](reference.md#environment).
