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
   its keys, and remembers the user for `orchard2 shell`
   ([remote.md](remote.md)); `--no-ssh` doesn't and doesn't ask.
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
- **The device is in recovery mode**, in a terminal: a guided run takes the
  steps' place, and the hold takes seconds. Enter starts it; the lit step
  counts down while you hold the buttons, and orchard2 restarts the device
  at the right moment. Esc (or q) stops it until that restart has gone out.
  When the device comes back in recovery mode instead, the steps stand
  again. DFU mode reached by hand counts as before.
- **The device is in recovery mode, without a terminal** (stdin or stderr
  is piped or redirected): nobody can press Enter, so the usual DFU steps
  show, and the run waits for DFU mode reached by hand. With `--json` the
  `recovery` events are written either way
  ([reference.md](reference.md#install-and-launch)).
- **More than one iPhone or iPad is connected**: nothing goes on until only
  one is. `--ecid` or `--location` names the one a run is for; then no other
  is found or started, but two connected still stop it
  ([devices.md](devices.md#one-device-for-install-and-launch)).
- **Another orchard2 works on the device**: it is shown as in use, and the
  run goes on when that has finished ([devices.md](devices.md#session)).
- **A start that fails** asks for DFU mode again and tries again.
- `--wait SECONDS` ends the run with exit status 3 when no device shows up
  in DFU mode in that time; 0, the default, waits for ever.

## Which devices

A board the orchard data marks supported or experimental. A board it does
not know, one it marks otherwise, and every 32-bit board is refused with
exit status 4 before anything is started on it. The release must cover the
board.

## How the device is entered

A **boot route** takes the device from DFU mode to the install system. Which
routes a device has follows from its chip, by `routes.toml` in the board
data; the first that fits is taken.

| Route | Is |
| --- | --- |
| `palera1n` | palera1n runs the boot ROM exploit and starts Pongo, which takes the install system |
| `checkra1n` | the same with checkra1n |
| `ibss` | gaster enters the boot ROM, and Apple's own iBSS (and iBEC, where the chip loads one), patched, takes the install system. The firmware is fetched from Apple, and its keys are read from the device the first time |

The data also names `autogo` (`--autogo`), the 32-bit boards' route into
Cascadia's loader, taken only when asked for by name. No command of this
version starts it yet.

`--boot ROUTE` names one, else `ORCHARD_BOOT`
([reference.md](reference.md#environment)). `--ibss` is short for
`--boot ibss`. A route that does not work on the device's chip ends the run
and says which do. palera1n and checkra1n are pinned by size and SHA-256 and
kept in the cache, as is gaster.

Resizing iPadOS starts it through palera1n, whatever the boot route.

Board data from before `routes.toml` has the one route there was then,
palera1n; to name another, use newer data (`--data DIR`).

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
