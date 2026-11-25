# To do

What is decided but not done, with why. Remove an entry when it is done.

## One way of working on a device

`install` and `launch` (and the installer's and the launcher's windows) run
a fixed path of their own in `crates/runtime`: palera1n or checkra1n into
Pongo, then m1n1. `prep`, `load` and `boot` in `crates/device` take a device
through any path the board data describes, inside a session.

The two are to become one. `install` and `launch` will run on the boot
commands' paths, which is what brings them:

- **Cascadia** (the 32-bit boards): `prep` and `boot` first, then in the
  installer.
- **iBSS and recovery** (a patched iBoot instead of Pongo) as another route
  in the installer.

Until then:

- **The lock is shared, the session is not.** Both sides take the same
  per-location lock (`orchard_cache::lock`), so only one piece of work
  touches a device; the runtime takes it from DFU mode on
  (`crates/runtime/src/held.rs`). What a session also carries, the device's identity
  across modes and a state USB cannot show (`crates/device/src/state.rs`),
  the runtime keeps to itself. At the consolidation the runtime's run
  becomes a session: one lock holder, one remembered state, the same
  guarantees for every command. Do not grow a second continuity mechanism
  in the runtime before that.
- **The app has no screen for a device in use.** The runtime lists it
  among the other devices with "in use by another orchard2" in place of its
  mode; the command line words that case, the windows show their "more than
  one device" screen, whose title and hint are wrong for it. The wording is
  Henrik's to decide.
- **`--via`, `--entry`, `--chain`, `--from`** belong to the boot commands.
  `install` and `launch` take none; `ORCHARD_ENTRY` is their only switch.
- **The boot paths' data** (`boot-paths.toml`, `boot.json` schema 2) is
  only on orchard's `boot-paths` branch, and the catalog is being
  rethought. Do not build on its present shape.
- **gaster has no pinned release** (`crates/device/src/entry.rs`), so every
  path through it, and `run`, stops as unsupported.

## Not run on a device yet

These came in with the cut-over from orchard's Python and were checked with
tests and the scripted demo only:

- `--ecid` and `--location` for `install` and `launch`;
- the lock in the runtime;
- `ORCHARD_ENTRY=checkra1n`.

Not run on Linux at all (written on macOS): the udev rule (`setup usb`, and
what `install` and `launch` install when access is refused), and `setup`'s
package installs with apt, dnf and pacman.
