# run

```sh
orchard run [DIRECTORY] [--boot ROUTE]
```

builds the project in DIRECTORY (this one by default), as
[`build`](build.md) does and with its options, and boots what it built in
RAM on the device in DFU mode. Nothing is installed.

The device is started by its boot route
([install.md](install.md#how-the-device-is-entered)): `--boot ROUTE`, or
`--ibss`, `--palera1n`, `--checkra1n`, `--autogo`. m1n1 then boots the kernel, which
goes up in one upload behind it where it fits. What is too large for a
patched iBoot's upload (more than 40 MiB by the `ibss` route) goes through
m1n1's proxy once it runs; `--proxy` asks for that way whatever the size.

| To | Give |
| --- | --- |
| boot the last build as it is | `--no-build` |
| build the project with a kernel tree of your own | `--kernel DIR`, as for `build` |
| boot files of your own | `--kernel FILE --dtb D… --initramfs I [--bootargs "…"]`: nothing is built |
| start m1n1 alone and leave it waiting on USB | `--bare` |
| use a local m1n1 | `--m1n1 FILE`: the plain build for palera1n and checkra1n, the Mach-O build for ibss |
| use a local loader of Cascadia's on the autogo route | `--loader FILE`, as for `build` |
| give the ibss route its firmware keys | `--keys FILE` (also `ORCHARD_FIRMWARE_KEYS`): see [Firmware keys](data-and-firmware.md#firmware-keys) |
| add the board's peripheral firmware to the initramfs | `--firmware` |
| end once the start has gone out, for a system that never shows up on USB | `--no-wait` |
| wait for DFU mode | `--wait SECONDS` |
| bound the device work | `--timeout SECONDS` (300) |

## The A12 boards

On the A12 boards (iPhone XR, XS, XS Max; iPad mini 5, iPad Air 3, iPad 8)
the `ibss` route is the one there is, and no tool of ours enters their boot
ROM either: a board running usbliter8 does, with the device plugged into it.
The device then shows as `usbliter8` under Pwned in `devices`, and from there
`orchard run` goes on as from gaster: Apple's iBSS, patched, then m1n1. The
iBSS takes any manifest, so no ticket of the device is needed.

## The 32-bit boards

On the A5 boards (iPad mini, iPad 2) the `ibss` route is the one there is,
and no tool of ours enters their boot ROM. The device has to be in DFU mode
with a boot ROM that checkm8 from a Raspberry Pi Pico entered (it shows as
`pwned checkm8` in `devices`), or in kDFU mode from a jailbroken iOS. From
there:

```sh
orchard run
orchard run --bare
orchard run --kernel zImage --dtb board.dtb --initramfs initramfs.cpio.gz
```

Apple's iBSS and iBEC are sent patched, the iBEC takes Apple's device tree
and m1n1's 32-bit build as its kernel, and `bootx` starts it. The 32-bit
m1n1 is Orchard's published one; `--m1n1 FILE` gives another. The firmware's keys are in
the board data (`--keys` gives others). The kernel is a zImage and goes up
behind m1n1; `--proxy` is not for these boards yet. Not run on a device.

### autogo

`--autogo` is the other route of these boards, Project Cascadia's, and the
one they are known to boot by. It needs no m1n1: the patched iBEC enters
what it is sent as soon as an upload ends, and is sent Cascadia's loader
with the kernel, the board's device tree and the initramfs.

```sh
orchard run --autogo
orchard run --autogo --kernel zImage --dtb board.dtb --initramfs initramfs.cpio.gz
```

The loader is the project's own build of it, `--loader FILE`, or the one
Orchard publishes for the board. Boot arguments are the device tree's;
`--bootargs` replaces them there. `--m1n1`, `--proxy` and `--bare` are for
the routes that end in m1n1. Both routes start the same patched iBSS and
iBEC the same way, so where `--ibss` fails and `--autogo` boots, the fault
is in m1n1. Not run on a device.

## A distribution checkout

In a distribution checkout (`orchard.toml` with `[release]`), `run` builds
the release and starts its installer system on the device from RAM, by the
same route and in the same way `install` starts it. Nothing is installed:
the system is there to look around in. `--no-build` takes the last build.
The options for a kernel of your own (`--m1n1`, `--proxy`, `--firmware`,
`--bootargs`, `--dtb`, `--initramfs`) are not for it.

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
loader = "/…/loader.bin"             # Cascadia's, on a 32-bit build
```

`--dtb`, `--initramfs`, `--bootargs` and `--firmware` on the command line
count over the file's. With several device trees, m1n1 takes the board's.

The run's log is `logs/run-TIME.log` in the cache
([reference.md](reference.md#the-run-log)); a failed run names it.

Each boot route is one function in `crates/runtime`, the same that
`install` and `launch` take, and follows the device through a session
([devices.md](devices.md#session)).
