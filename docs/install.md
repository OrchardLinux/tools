# install

```sh
orchard2 install NAME
```

installs a distribution on the iPad or iPhone that is put in DFU mode. It is
the graphical installer's flow on a terminal: it shows the same steps, asks
the same questions, and takes an option where a script cannot answer.

## The source

`orchard2 install [SOURCE]`, or `--index SOURCE`:

| SOURCE | Is |
| --- | --- |
| a distribution's name | the newest release of `OrchardLinux/NAME` on GitHub (its `index.toml`) |
| an `https://` URL | a release index, or one release's manifest |
| a file | the same, with the files it names read from beside it |
| nothing, in a distribution checkout | the release its last `orchard2 build` made |

A release index (`kind = "orchard.index"`) pins one release's manifest by
size and SHA-256; a manifest (`kind = "orchard.install"`) pins every file of
the release. Each file is checked before it is used.

## What happens

1. **DFU mode.** The run waits for a device in DFU mode and shows how to get
   there ([Waiting for DFU mode](#waiting-for-dfu-mode)).
2. **The storage.** The system goes into the space after iPadOS.
   `--split [GB]` resizes iPadOS first to make that space: with a number it
   is the system's size, without one it is asked. With a terminal and no
   `--split`, the run asks whether to resize. Resizing starts iPadOS once
   with a helper that does it, and asks for DFU mode again afterwards.
3. **Erasing.** Partitions after iPadOS are erased. They are listed, and the
   run goes on after a typed `yes`, or with `--yes`.
4. **The account.** The user's password (8 characters or more) and the lock
   screen PIN (6 digits) are asked twice each, or read from
   `--password-file FILE` and `--pin-file FILE` (`-`: the next line of
   stdin, password first). `--ssh` lets this computer sign in over SSH with
   its keys; `--no-ssh` doesn't and doesn't ask.
5. **Writing.** The system is written and finished on the device, which then
   starts it.

A storage that is not split, with no `--split`, ends the run with exit
status 5 and says so.

## Without a terminal

Nothing is asked. What no option answers ends the run with exit status 5 and
names the option: `--yes`, `--password-file`, `--pin-file`, `--split GB`.
Resizing and SSH are off unless their options are given.

```sh
orchard2 --json install NAME --yes --no-ssh \
    --password-file pw --pin-file pin --wait 600
```

## Waiting for DFU mode

- **Nobody has seen the device yet**: the steps stand under three tabs, as
  in the installer: Home button, No Home button, iPhone 7 and 7 Plus. ← and
  → change the tab (n, 7 and h name one); t shows tips.
- **The device is connected in another mode** (running iPadOS, recovery
  mode, the install system): its name, that mode, and the steps for its own
  buttons.
- **More than one iPhone or iPad is connected**: nothing goes on until only
  one is. `--ecid` or `--location` names the one a run is for; then no other
  is found or started, but two connected still stop it
  ([devices.md](devices.md#one-device-for-install-and-launch)).
- **A start that fails** asks for DFU mode again and tries again.
- `--wait SECONDS` ends the run with exit status 3 when no device shows up
  in DFU mode in that time; 0, the default, waits for ever.

## Which devices

A board the orchard data marks supported or experimental. A board it does
not know, one it marks otherwise, and every 32-bit board is refused with
exit status 4 before anything is started on it. The release must cover the
board.

## How the device is entered

From DFU mode palera1n runs the boot ROM exploit and starts Pongo, on every
board. `ORCHARD_ENTRY=checkra1n` has checkra1n do it instead
([reference.md](reference.md#environment)). Both are pinned by size and
SHA-256 and kept in the cache. `install` has no option for it: choosing
entries and chains is the boot commands'
([boot-commands.md](boot-commands.md)).

## What it uses

- The release's root image, platform layer, install tools and logo.
- Orchard's Pongo and m1n1, and the board data: orchard's newest release's,
  or `--data DIR`.
- The kernel package of a pacman-based release, from OrchardLinux/packages.
- The board's peripheral firmware, built from Apple's firmware by its recipe
  ([data-and-firmware.md](data-and-firmware.md)).

The run's log is `--log FILE`, else `logs/cli-TIME.log` in the cache.

## Ctrl-C

At a question it ends the run at once. While the storage is split or the
system is written the first press only warns. Otherwise it ends the run; a
device still running the resize helper is restarted first. Exit status 8.

Events and exit statuses: [reference.md](reference.md#install-and-launch).
