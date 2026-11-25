# Talking to a device directly

One command per protocol, each doing one thing to the device in that mode.
They take `--timeout SECONDS` (30) per action, name a device like every
command ([devices.md](devices.md#naming-a-device)), and take its lock.

## dfu

The boot ROM's DFU protocol.

| Command | Does |
| --- | --- |
| `dfu status` | prints the DFU state and status |
| `dfu clear-status` | clears an error status; the device returns to idle |
| `dfu abort` | abandons a transfer in progress; the device returns to idle |
| `dfu send FILE` | sends an image, which the boot ROM runs: the device comes back as whatever the image is |
| `dfu reset` | resets the device's USB port |

## recovery

iBoot, in recovery mode.

| Command | Does |
| --- | --- |
| `recovery send FILE` | uploads a file into iBoot's load buffer |
| `recovery command "LINE"` | runs one iBoot command |
| `recovery getenv NAME` | prints one environment variable |

## pongo

PongoOS's shell.

| Command | Does |
| --- | --- |
| `pongo output` | prints the shell's new output and whether it is busy |
| `pongo send FILE` | uploads a file into Pongo's load buffer |
| `pongo command "LINE"` | sends one command line to the shell |

## m1n1

m1n1's proxy. Numbers are decimal or `0x` hex.

| Command | Does |
| --- | --- |
| `m1n1 ping` | checks that the proxy answers |
| `m1n1 call OPERATION [ARGS…]` | runs one proxy operation with up to six arguments and prints its result |
| `m1n1 read ADDRESS SIZE` | writes device memory to stdout (`--json`: address, size, SHA-256) |
| `m1n1 write ADDRESS FILE` | writes a file into device memory |
| `m1n1 pack FILE --kernel K --dtb D… [--initramfs I] [--bootargs "…"] [--logo L] -o OUT` | packs m1n1 and what it is to boot into one image; needs no device |

A command that the device cannot confirm (it is gone once the code runs)
reports `unconfirmed` instead of `acknowledged`.
