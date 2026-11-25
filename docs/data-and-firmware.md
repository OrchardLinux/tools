# Board data and firmware

## The orchard data

What orchard2 knows about a device it reads from the orchard data: one
directory per board (`devices/BOARD/`), the chips, the platforms and the
features.

- `--data DIR` or `ORCHARD_DATA` uses a directory as it is: an orchard
  checkout, or anything with `devices/` in it.
- Without it, a copy of orchard's main branch in the cache
  (`data/current`), fetched the first time it is needed and then used as it
  is, however old. `orchard2 data update` fetches it again.

`install` and `launch` differ: without `--data` they use the board data of
orchard's newest release, with its Pongo and m1n1. `build` takes its
recipes from the newest nightly ([build.md](build.md#recipes)).

```sh
orchard2 data            # where the data comes from: main at which commit, or local
orchard2 data update     # fetch main now
```

`data` also names the loaders that are current: the m1n1, Pongo and
Cascadia loader of orchard's newest nightly, each by its key. `data update`
refreshes that record, and the build's recipes when there is a copy of
them.

## boards, board

```sh
orchard2 boards          # every board: name, chip, status, display name
orchard2 board j71b      # one board
```

`board NAME` prints the board's products, chip and board number, support
status, boot chains, kernel platform and device tree, the firmware its
recipes are for (version, build, number of files), and its features with
the state of each (`working`, `untested`, `missing`, `none`).

## fw

A board needs two kinds of firmware from Apple's own files (IPSWs), and the
data holds a **recipe** for each: which files to take and how to turn them
into what is needed.

- **boot**: what a boot chain sends to bring up a patched iBoot;
- **peripheral**: what Linux loads for the board's Wi-Fi, Bluetooth, touch
  and so on.

`--type boot|peripheral` chooses (boot by default). FIRMWARE is a board
name (its recipe's firmware), an `https://` URL or a local file.

| Command | Does |
| --- | --- |
| `fw info FIRMWARE` | the version and build, the boards it covers, and with a board name the recipe's inputs |
| `fw ls FIRMWARE [PATH]` | the archive's members; with `--image COMPONENT` a directory inside that disk image |
| `fw cat FIRMWARE PATH` | one recipe input, archive member, or file inside a disk image, to stdout |
| `fw build BOARD --out DIR` | builds the board's firmware into DIR |

`--board` and `--volume` say which when a firmware covers several boards or
a disk image has several volumes. Only the parts that are read are fetched,
and they are kept in the cache by their SHA-256.

`fw build`:

- `--chain NAME` (boot): the chain to build for; the board's only one by
  default. `--keys` and `--im4m` as for
  [prep](boot-commands.md#firmware-keys).
- `--device-keys` (boot): takes missing keys from the device, which must be
  that board in DFU mode; this may run gaster on it. With every key in the
  cache no device is touched.
- `--skip-unsupported` (peripheral): builds what this version can and lists
  what it cannot make, instead of failing.

`install` builds a board's peripheral firmware by itself.
