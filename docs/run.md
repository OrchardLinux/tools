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
| give the ibss route its firmware keys | `--keys FILE` (also `ORCHARD_FIRMWARE_KEYS`): see [Firmware keys](data-and-firmware.md#firmware-keys) |
| add the board's peripheral firmware to the initramfs | `--firmware` |
| end once the start has gone out, for a system that never shows up on USB | `--no-wait` |
| wait for DFU mode | `--wait SECONDS` |
| bound the device work | `--timeout SECONDS` (300) |

## The 32-bit boards

On the A5 boards (iPad mini, iPad 2) the `ibss` route is the one there is,
and no tool of ours enters their boot ROM. The device has to be in DFU mode
with a boot ROM that checkm8 from a Raspberry Pi Pico entered (it shows as
`pwned checkm8` in `devices`), or in kDFU mode from a jailbroken iOS. From
there:

```sh
orchard2 run --bare --m1n1 m1n1-armv7.macho --keys keys.json
orchard2 run --m1n1 m1n1-armv7.macho --keys keys.json \
    --kernel zImage --dtb board.dtb --initramfs initramfs.cpio.gz
```

Apple's iBSS and iBEC are sent patched, the iBEC takes Apple's device tree
and m1n1's 32-bit build as its kernel, and `bootx` starts it. No 32-bit
m1n1 is published yet, so `--m1n1` is needed, and the firmware's keys come
from `--keys` (they are public for these boards). The kernel is a zImage
and goes up behind m1n1; `--proxy` is not for these boards yet. Not run on
a device.

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
