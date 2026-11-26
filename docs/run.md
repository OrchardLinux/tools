# run

```sh
orchard2 run [SOURCE] [--kernel K --dtb D… --initramfs I] [--bootargs "…"] [--boot ROUTE]
```

starts m1n1 on the device in DFU mode, on any board, by its boot route
([install.md](install.md#how-the-device-is-entered)): `--boot ROUTE`, or
`--ibss`, `--palera1n`, `--checkra1n`.

- **Without a kernel** m1n1 is started and waits on USB.
- **With a kernel**, its device tree and an initramfs, m1n1 boots that
  Linux in RAM. They go up in one upload behind m1n1 where they fit. What
  is too large for a patched iBoot's upload (more than 40 MiB by the
  `ibss` route) goes through m1n1's proxy once it runs; `--proxy` asks for
  that way whatever the size.

`--firmware` adds the board's peripheral firmware to the initramfs,
`--m1n1 FILE` uses a local m1n1 instead of the published one (the plain
build for palera1n and checkra1n, the Mach-O build for ibss), `--wait
SECONDS` waits for DFU mode, and `--timeout SECONDS` (300) bounds the
device work.

**The shortcut.** `orchard2 run SOURCE` reads the same options from a
`run.toml`: the file itself, the one in a directory, or the one `orchard2
build` left in a project for the board's platform
(`out/PLATFORM/run.toml`). So `orchard2 build && orchard2 run .` boots what
was built.

```toml
kernel = "/…/Image.gz"
dtb = ["/…/t8010-j71b.dtb", "/…/t8010-j171.dtb"]
initramfs = "initramfs.cpio.gz"      # counted from the file's directory
bootargs = "console=tty0 loglevel=7 rdinit=/init"
firmware = true                      # as --firmware
```

An option given on the command line counts over the file's. With several
device trees, m1n1 takes the board's.

Each boot route is one function in `crates/runtime`, the same that
`install` and `launch` take, and follows the device through a session
([devices.md](devices.md#session)).
