# Changelog

What changed for people who use Orchard's tools: the command line (`orchard`)
and the installer and launcher apps. Every pull request that changes what a
user sees or can do adds a line under `[Unreleased]`, in the section of the
program it changes. When releasing, rename `[Unreleased]` to the version and
date; the release workflow puts that section into the GitHub release notes
and fails if it is missing or empty.

## [Unreleased]

### Command line

- Firmware recipes can extract the A7's GPU firmware: step
  `agx-g3-ldr-from-kernelcache@1` takes AGXFirmwareKextG3's META LDR from an
  iOS 12 kernelcache, found as the A8's is (the A7 boards' recipes in orchard
  pin it as `apple/gfx-s5l8960x.bin`).
- The command line is called `orchard` now (it was `orchard2`) and replaces
  orchard's Python tool. Each release carries it for macOS and Linux. Install it with
  `curl -fsSL https://raw.githubusercontent.com/OrchardLinux/tools/main/install.sh | sh`;
  `orchard update` fetches a newer one.
- `install` and `launch` take `--boot ROUTE` to choose how the device starts
  (palera1n, checkra1n, or the ibss route through gaster), from the boards'
  `routes.toml`. The ibss route also runs on the 32-bit boards and, from a
  boot ROM that usbliter8 entered, on the A12.
- `install` and `launch` take `--m1n1 FILE`, a local m1n1 instead of the
  published one, for boards whose m1n1 support is not published yet. A file
  of the wrong kind for the route is refused before anything is sent.
- `install NAME --kernel DIR` installs a release with a kernel built from
  your own tree; with `--data` and `--m1n1` that also covers a board no
  release lists yet. The kernel tree option is `--kernel` everywhere now
  (`--kernel-tree` still works), and `run --kernel` takes a tree or a
  finished image.
- On the A12 boards Apple's boot loader is no longer asked for a verbose
  start, as on every other chip.
- `run` in a directory with nothing to run says so and names what works
  from anywhere.
- `install`, `launch` and `run` take `--busy-cpu` (Linux, off by default):
  for computers where checkm8 only gets in under load, the exploit runs on
  one core while the others are kept busy, until PongoOS is up or, by the
  ibss route, gaster is in the boot ROM.
- On Linux the USB port reset after checkm8 waits 10 s instead of 2.5 s, so
  it no longer comes while the device is returning by itself.
- `install` works on the 32-bit boards: it reads their LwVM disk, offers the
  free space, and adds a boot partition and a root.
- With the device in recovery mode, `install` and `launch` guide it into
  DFU mode step by step.
- `install` says "Still downloading NAME" while the device is ready and
  the system is not there yet, and starts the install when it is, instead
  of standing at "Writing the system 0%".
- `run` starts a RAM boot on every board by its route; `run SOURCE` takes
  the options `build` wrote; `--no-wait` ends once the start has gone out;
  in a distribution checkout it starts that distribution's installer system.
  A first payload takes the nightly's kernel instead of compiling one.
- `resize` puts Resize APFS on the device and starts it, through palera1n,
  checkra1n or, on a Mac, Dopamine (signed with Xcode or xtool).
- `ssh-access` sets up SSH for a system that was installed without it.
- `capture-adt` reads Apple's device tree from a device a RAM payload runs
  on, for bringing up a board.
- On Linux without USB devices, `orchard` says why; in WSL, how Windows
  hands the device over.
- A failed download or unusable file is reported in the same words as the
  installer window.
- `launch` says when a device has nothing installed.
- Firmware recipes can take a kext's firmware from an iOS 16 or later
  kernelcache too, and the new step `section-from-kernelcache@1` takes any
  one section of a kext whole (`bundle`, `segment`, `section`): the
  iPhone XR's PMP firmware comes this way.

- `install`, `launch` and `run` keep one run log, named by when the run
  started (`logs/2026-11-28-114812-run.log` in the cache): orchard's own steps and
  the output of palera1n, checkra1n and gaster in one file, in order, each
  line with the seconds since the start and who said it. It shows every
  step of the USB port reset, the busy cores, what USB showed and why a tool
  was ended. A cancelled run keeps its log up to there, the ECID is left
  out, a failed `run` ends with the log's path, and a run that worked
  has its result as the log's last line.
- By the ibss route the run log shows each stage gaster tries and whether
  it worked; until now gaster said nothing while it entered the boot ROM.

### Installer and launcher

- With the device in recovery mode, the window guides it into DFU mode in
  five steps, with a countdown on the step that is running.
- Create your account says "Still downloading NAME." and keeps Install off
  while the device is ready and the system is not there yet, instead of
  going on to "Writing the system" at 0%.
- `ORCHARD_BUSY_CPU=1` turns on `--busy-cpu` for the windows; the longer USB
  port reset on Linux applies to them too.
- The launcher's program file is `orchard-launcher`, so it no longer has the
  command line's name.
- Bundle identifiers are under `org.orchardlinux`.
- The window's log is the same run log as the command line's, with the
  tools' output in it.

## [0.1.5] - 2026-11-25

Releases up to 0.1.5 have no changelog entries; their notes are on the
[releases page](https://github.com/OrchardLinux/tools/releases).
