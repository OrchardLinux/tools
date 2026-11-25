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

## Firmware of the 32-bit boards

To do with the Cascadia flow, not before: reading a 32-bit board's firmware
inputs, so that `fw build p105 --type peripheral` and then the installer
make its files.

Today it stops at the first input: a 32-bit restore image's root filesystem
is an encrypted disk image and its kernel an encrypted IMG3, and both need
the board's firmware keys. What is missing is small, because the means to
get the keys are here already: the boot firmware of the 32-bit chains is
built from IMG3 with keys from the key store, a file, the board data or the
device (`crates/boards/src/boot_firmware.rs`, `data/firmware_keys.rs`,
`crates/formats/src/img3.rs`). To add, after orchard's Python
(`legacy_images.py`, `remote_ipsw.py`):

- the kernel input: an IMG3 kernelcache decrypted with its key and
  decompressed (`img3_kernelcache`);
- the root filesystem inputs: the encrypted disk image read at random
  (`Encrcdsa`), under the image reader the 64-bit boards use;
- the keys for both from the same sources as the boot firmware's.

The step that follows is ported and checked against Python on a made-up
kernel (`sgx543-from-kernelcache@1`); p105's real file has not been made.
`install` refuses 32-bit boards before it gets there, so nobody meets this
yet.

## Not run on a device yet

These came in with the cut-over from orchard's Python and were checked with
tests and the scripted demo only:

- `--ecid` and `--location` for `install` and `launch`;
- the lock in the runtime;
- `ORCHARD_ENTRY=checkra1n`;
- `shell`, `exec`, `push` and `pull` (`crates/remote`): the serial console
  protocol is tested against a scripted console, the rest by what it
  refuses. No payload and no installed system has been reached with them.

Not run on Linux at all (written on macOS): the udev rule (`setup usb`, and
what `install` and `launch` install when access is refused), and `setup`'s
package installs with apt, dnf and pacman.
