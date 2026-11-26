# run

```sh
orchard2 run [DIRECTORY] [--boot ROUTE]
```

builds the project in DIRECTORY (this one by default), as
[`build`](build.md) does and with its options, and boots what it built in
RAM on the device in DFU mode. Nothing is installed.

The device is started by its boot route
([install.md](install.md#how-the-device-is-entered)): `--boot ROUTE`, or
`--ibss`, `--palera1n`, `--checkra1n`. m1n1 then boots the kernel, which
goes up in one upload behind it where it fits. What is too large for a
patched iBoot's upload (more than 40 MiB by the `ibss` route) goes through
m1n1's proxy once it runs; `--proxy` asks for that way whatever the size.

| To | Give |
| --- | --- |
| boot the last build as it is | `--no-build` |
| boot files of your own | `--kernel K --dtb D… --initramfs I [--bootargs "…"]`: nothing is built |
| start m1n1 alone and leave it waiting on USB | `--bare` |
| use a local m1n1 | `--m1n1 FILE`: the plain build for palera1n and checkra1n, the Mach-O build for ibss |
| add the board's peripheral firmware to the initramfs | `--firmware` |
| wait for DFU mode | `--wait SECONDS` |
| bound the device work | `--timeout SECONDS` (300) |

A distribution checkout is refused: its release is installed
([install.md](install.md)), not run.

## run.toml

A build leaves `run`'s options for what it built in
`out/PLATFORM/run.toml`, which is what `run` then reads. DIRECTORY can also
be such a file, or a directory with one in it.

```toml
kernel = "/…/Image.gz"
dtb = ["/…/t8010-j71b.dtb", "/…/t8010-j171.dtb"]
initramfs = "initramfs.cpio.gz"      # counted from the file's directory
bootargs = "console=tty0 loglevel=7 rdinit=/init"
firmware = true                      # as --firmware
```

`--dtb`, `--initramfs`, `--bootargs` and `--firmware` on the command line
count over the file's. With several device trees, m1n1 takes the board's.

Each boot route is one function in `crates/runtime`, the same that
`install` and `launch` take, and follows the device through a session
([devices.md](devices.md#session)).
