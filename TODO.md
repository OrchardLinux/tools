# To do

What is decided but not done, with why. Remove an entry when it is done.

## Routes

`install`, `launch` and `run` take a boot route: one function each in
`crates/runtime` (`chain.rs`: palera1n and checkra1n into Pongo; `ibss.rs`:
gaster and a patched iBoot), chosen from `routes.toml` in the board data,
and following the device through a session (`crates/device`). The planner
that `prep`, `load` and `boot` ran on is gone. Still to do:

- **The 32-bit boards** (Cascadia's): a route of their own, a patched iBEC
  that waits, Apple's device tree, m1n1's 32-bit build
  (`formats::m1n1::kernel_image_arm`) and `bootx`. With it, `install` and
  `launch` can stop refusing them.
- **Access routes** (`access.*` in `routes.toml`): `resize` and the
  installer app on palera1n, checkra1n and Dopamine. Today the storage is
  split through palera1n only (`chain::boot_ipados`); checkra1n's and
  Dopamine's ways are in resize-apfs's scripts.
- **orchard's `run`** handed over to `orchard2 run` where orchard2 is
  switched on (orchard's `tools/launcher`).
- **The old data formats.** `boards` still reads `boot-paths.toml` into the
  copy of the data and `boot.json` schema 2 (chains with patch steps,
  `loaders.cascadia`). Only the 32-bit autogo chain and the A12 and A13
  patches use the steps. Decide with the 32-bit route what of it stays.
- **A device in use has words, not a screen of its own.** The flow gives
  the title and the hint (`Flow::in_use`), and the windows show them on the
  "more than one device" screen's layout. Not looked at in a window yet.
- **The USB session files' old place.** `build` reads `usb-session` and
  `usb-identity` from `boot/session/` in the data and still falls back to
  `src/orchard/` (`crates/build/src/ram.rs`). Drop the fallback once a
  nightly has them in `boot/session/`.

## macOS asks before a new accessory connects

Seen on a MacBook on 2026-11-27, twice: the first time a device shows up
with USB ids this Mac has not seen at that port (Pongo, then a RAM-booted
Linux), macOS asks "Allow accessory to connect?". Until it is answered the
device is in the system's USB list (`ioreg -p IOUSB`) but is not handed to
programs: `devices` does not list it, and `run` waited its whole time for
Linux and ended with a timeout while Linux was up.

To do: tell the two apart and say so ("macOS is asking whether to allow
this accessory; click Allow") in `devices`, in a route's waits and on the
windows' screens. What marks a held-back device in the registry has to be
read off one while the question is open; the next new identity (the install
system, an installed system) will ask again.

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

## What orchard's Python still does

Looked at on 2026-11-26, for the day the Python tool goes:

- `run`'s start of what was built, until orchard hands it to `orchard2
  run` (above), and `boot` (a release's recovery or an image, in RAM,
  without installing). Releases have had no recovery since
  about 0.3 (Henrik), so the distribution-checkout form of both may have
  nothing left to boot; check before porting it.
- `python -m orchard.published keys`: orchard's build.yml writes
  `component-keys.json` with it, and keeps doing so: CI does not use
  orchard2 (Henrik, 2026-11-26). orchard2 reads that document, and its
  hidden `component-keys` writes the same one, for comparing.
- `python -m orchard.installer release` and `core`: `install.sh` with
  `orchard-install.pyz` in every distribution release, and
  `orchard-core.pyz` for `orchard update`. They ship the Python tool and go
  with it; before that, orchard2 needs its own way to be installed
  (`curl | sh`, perhaps a brew tap) and to update itself. To be discussed
  with Henrik; `adopt` goes with them.
- `resolve` (download and verify what a published release needs for a
  board, without a device): named in omarchy-a's release checklist for a
  person to run. orchard's verify-public.yml also runs it, but that
  workflow is stale (old wheel pair, renamed repositories, expects a
  recovery) and CI does not use orchard2. Not ported; the installer does
  the same work only with a device attached.
- `mixin`, `setup-host`: no use found anywhere.
- `distros`: undecided.

## Not run on a device yet

These came in with the cut-over from orchard's Python and were checked with
tests and the scripted demo only:

- `--ecid` and `--location` for `install` and `launch`;
- the guided run from recovery mode in `install` and `launch` (the steps,
  Enter and Esc, the countdown): seen in the demo
  (`ORCHARD_DEMO_RECOVERY=1 orchard2 launch --demo`), not with a device;
- the lock in the runtime;
- the `checkra1n` boot route, and `install` by the `ibss` route (`launch`
  by it reached m1n1 on a j71b);
- `devices` asking a device that runs iOS for its name and ECID;
- that `install --ssh` knows the unit (`user-UNIT`, the old host key
  forgotten): it takes the id from the install environment's USB serial,
  else makes it from the serial number seen in recovery mode. DFU mode
  gives no serial number (seen on a j71b). Unchecked: that the install
  environment's serial has `ID:`, and that recovery's `SRNM` is the device
  tree's `serial-number`. If neither holds, a reinstalled unit's old host
  key stays and the error says which line to remove;
- `capture-adt`: the parser and the partition lookup are tested on made
  trees and tables; no real tree was captured. Its check of the unit id
  on USB against the tree's serial number is a note, not a refusal, until
  one capture shows it holds;
- `ssh-access`: the login is tested against a scripted one, never against
  a system's real serial login;
- `shell`, `exec`, `push` and `pull` (`crates/remote`): the serial console
  protocol is tested against a scripted console, the rest by what it
  refuses. No payload and no installed system has been reached with them.

Not run on Linux at all (written on macOS): the udev rule (`setup usb`, and
what `install` and `launch` install when access is refused), and `setup`'s
package installs with apt, dnf and pacman.
