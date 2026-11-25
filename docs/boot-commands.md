# The boot commands

`prep`, `load` and `boot` take a device through a boot chain step by step,
on any board the orchard data describes a path for. `install` and `launch`
do not use them yet; they have a fixed path of their own.

> **State of this part.** These commands read the boot paths from
> `boot-paths.toml` and each board's chain recipes from its `boot.json`
> (schema 2) in the orchard data. orchard's main branch does not carry
> those yet (they are on its `boot-paths` branch), so with the default data
> the commands find no path. Point `--data` at data that has them.

## States

A device is in one **state**. USB shows some of them; the others have to be
remembered (a [session](devices.md#session) does) or stated with `--from`.

| State | Is | USB shows it |
| --- | --- | --- |
| `dfu` | the boot ROM, untouched | yes |
| `pwned-dfu` | the boot ROM after an exploit, which leaves its marker | yes |
| `kdfu` | a patched iBSS started from jailbroken iOS | as `dfu` |
| `jailbroken-ios` | iOS, jailbroken | no |
| `pongo` | PongoOS | yes |
| `patched-recovery` | a patched iBoot that takes m1n1 | as `recovery` |
| `autogo` | an iBEC that starts the loader it is sent | as `recovery` |
| `m1n1` | m1n1's proxy | yes |
| `linux` | Linux | yes |

A device in recovery mode that nothing is known about is `ambiguous`: state
it with `--from patched-recovery` or `--from autogo`, or put it in DFU mode.

## Entries, chains, loaders

A path from where the device is to running code is made of **methods**,
listed in order of preference in `boot-paths.toml`:

- an **entry** is an outside tool that gets into the device: `palera1n` and
  `checkra1n` (DFU to Pongo), `gaster`, `usbliter8`, `pico-checkm8` (DFU to
  a pwned boot ROM), `kdfu` (jailbroken iOS to kdfu). This version runs
  palera1n, checkra1n and gaster itself. For the others it says what to do
  by hand; a chain can then start from the state they leave.
- a **chain** is firmware orchard2 builds and sends to bring up a patched
  iBoot: `iboot` (64-bit: a pwned boot ROM to `patched-recovery`) and
  `iboot-autogo` (32-bit: a pwned boot ROM or kdfu to `autogo`).
- a **loader** starts code on a receiver: `m1n1` (from Pongo or a patched
  iBoot) and `cascadia` (from an autogo iBEC, 32-bit).

A **receiver** is a state that waits for a load: Pongo, a patched iBoot, an
autogo iBEC.

Which methods fit a board follows from its chip: the entries that list it,
and the chains and loaders of its architecture that the board has a recipe
for. The first path that fits is taken. To choose:

| Option | Chooses |
| --- | --- |
| `--from STATE` | the state the device is in, where USB does not show it |
| `--entry NAME` | the entry tool, where several fit |
| `--chain NAME` | the chain, where several fit |
| `--board NAME` | the board, where the device does not report it |

palera1n and checkra1n need exactly one untouched device in DFU mode:
`--ecid` and `--location` cannot choose one of two for them.

## prep

```sh
orchard2 prep [--plan]
```

leaves the device in a receiver. It works out the state and the board,
picks the path, makes every file before the device is touched (Pongo, the
chain's firmware), fetches the entry's tool, and runs the steps. A receiver
USB cannot tell apart is remembered in the session.

`--plan` lists the steps and touches nothing; with `--board` and `--from`
it does not even look for a device:

```
j72 in dfu
checkra1n (entry): dfu to pongo
  1. run checkra1n -E -p -v -n -k pongo.bin
  2. wait pongo
```

A run prints each method and step with how it ended (`done`,
`acknowledged`, `unconfirmed`), then the state the device is in.
`--timeout` (300 s) bounds all steps together.

### Firmware keys

A chain's firmware is built from Apple's, which is encrypted. The keys
come, in this order, from:

1. `--keys FILE`, else `ORCHARD_FIRMWARE_KEYS`: a JSON file that is yours
   alone (mode 0600). With one given, nothing else is looked at.
2. the cache (`firmware-keys/`), private to you;
3. the board's `boot.json`, where the keys are public;
4. the device itself, through gaster, from a boot ROM gaster pwned
   (`fw build --device-keys` does only this step).

`--im4m FILE` is the manifest 64-bit firmware is signed with; without it a
pinned one for the chip is fetched and checked.

## load

```sh
orchard2 load FILE [--kernel K --dtb D… --initramfs I --bootargs "…" --logo L]
```

puts code on the device and starts it. It runs `prep` itself when the
device is not in a receiver yet, and uses the receiver it already waits in
when that fits. What FILE is decides the loader:

| FILE | Goes to | And then |
| --- | --- | --- |
| a plain image | Pongo | `bootm`; the device leaves Pongo |
| m1n1's Mach-O | a patched iBoot | m1n1 shows up on USB |
| Cascadia's loader | an autogo iBEC | Linux shows up on USB |

With `--kernel`, the kernel, device trees (`--dtb`, repeatable, in order),
initramfs, command line and logo are packed behind m1n1, or into
Cascadia's bundle (which takes exactly one device tree and needs an
initramfs). `--wait MODE` also waits for the device in that mode after a
Pongo load. `--plan` and the options of `prep` apply. A device already in
m1n1 or Linux is refused.

`orchard2 m1n1 pack` makes the same image as a file, without a device
([low-level.md](low-level.md#m1n1)).

## boot

```sh
orchard2 boot [--list | --boot-entry ID]
```

starts a system installed on the device. The device is taken into m1n1 (by
`prep`'s path, unless it is there already), and m1n1's boot manager does
the rest: it reads the device's storage, finds the boot entries, checks
them, and loads and starts one. The computer reads nothing off the disk and
writes nothing.

- `--list` prints the entries (id, title, version, whether each can boot
  and why not) and starts none.
- `--boot-entry ID` starts that one; without it, the first that can boot.
- `--dtb-name NAME` is the device tree's file name, which an entry has to
  bring; it defaults to the board's.
- `--no-splash` leaves out the system's picture while it loads.
- `--wait MODE`, `--plan` and the options of `prep` apply.

`orchard2 launch` is this for people: the launcher's flow, with its
questions and steps ([launch.md](launch.md)).

## run

```sh
orchard2 run --kernel K --dtb D --initramfs I
```

is an older, single-purpose command: it boots Linux in RAM on a J71b (iPad
6) through gaster and a patched iBoot, then uploads the kernel through
m1n1's proxy. It knows no other board or chain, has no `--plan`, and needs
gaster. `load` is the general way.

`--bootargs "…"` is the kernel's command line, `--firmware` adds the
board's peripheral firmware to the initramfs, `--m1n1 FILE` uses a local
m1n1 instead of the published one, `--keys FILE` and `--im4m FILE` replace
the firmware keys and the manifest, and `--timeout SECONDS` (300) bounds
the device work.
