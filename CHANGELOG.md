# Changelog

What changed for people who use Orchard's tools: the command line (`orchard`)
and the installer and launcher apps. Every pull request that changes what a
user sees or can do adds a line under `[Unreleased]`, in the section of the
program it changes. When releasing, rename `[Unreleased]` to the version and
date; the release workflow puts that section into the GitHub release notes
and fails if it is missing or empty.

## [Unreleased]

### Command line

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
- `run` in a directory with nothing to run says so and names what works
  from anywhere.
- `install`, `launch` and `run` take `--busy-cpu` (Linux, off by default):
  for computers where checkm8 only gets in under load, the exploit runs on
  one core while the others are kept busy.
- On Linux the USB port reset after checkm8 waits 10 s instead of 2.5 s, so
  it no longer comes while the device is returning by itself.
- `install` works on the 32-bit boards: it reads their LwVM disk, offers the
  free space, and adds a boot partition and a root.
- With the device in recovery mode, `install` and `launch` guide it into
  DFU mode step by step.
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

### Installer and launcher

- With the device in recovery mode, the window guides it into DFU mode in
  five steps, with a countdown on the step that is running.
- `ORCHARD_BUSY_CPU=1` turns on `--busy-cpu` for the windows; the longer USB
  port reset on Linux applies to them too.
- The launcher's program file is `orchard-launcher`, so it no longer has the
  command line's name.
- Bundle identifiers are under `org.orchardlinux`.

## [0.1.5] - 2026-11-25

Releases up to 0.1.5 have no changelog entries; their notes are on the
[releases page](https://github.com/OrchardLinux/tools/releases).
