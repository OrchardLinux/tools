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
- **A device in use has words, not a screen of its own.** The flow gives
  the title and the hint (`Flow::in_use`), and the windows show them on the
  "more than one device" screen's layout. Not looked at in a window yet.
- **`--via`, `--entry`, `--chain`, `--from`** belong to the boot commands.
  `install` and `launch` take none; `ORCHARD_ENTRY` is their only switch.
- **The boot paths' data** (`boot-paths.toml`, `boot.json` schema 2) is
  only on orchard's `boot-paths` branch, and the catalog is being
  rethought. Do not build on its present shape.
- **gaster has no pinned release** (`crates/device/src/entry.rs`), so every
  path through it, and `run`, stops as unsupported. There is nothing to pin
  yet: OrchardLinux/gaster's tag `orchard-worker-v0.1.0` has no release,
  because its build never got a macOS arm64 runner (run 45233948167; the
  other three hosts built). Rerun it, publish, then pin the four files by
  size and SHA-256.
- **The USB session files' old place.** `build` reads `usb-session` and
  `usb-identity` from `boot/session/` in the data and still falls back to
  `src/orchard/` (`crates/build/src/ram.rs`). Drop the fallback once a
  nightly has them in `boot/session/`.

## Not run on a device yet

These came in with the cut-over from orchard's Python and were checked with
tests and the scripted demo only:

- `--ecid` and `--location` for `install` and `launch`;
- the lock in the runtime;
- `ORCHARD_ENTRY=checkra1n`.

Not run on Linux at all (written on macOS): the udev rule (`setup usb`, and
what `install` and `launch` install when access is refused), and `setup`'s
package installs with apt, dnf and pacman.
