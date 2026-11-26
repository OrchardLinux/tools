# In-app installer

An iPhone or iPad app that installs a distribution from inside iOS, with no
computer driving it: it splits the storage, downloads the release and writes
it. For devices that cannot be started from DFU mode it is the only way to
install; starting the installed system is not its part.

| Path | What it is |
| --- | --- |
| `App` | The app (UIKit, iOS 12): it shows what the helper tells it and sends what the user does |
| `Helper` | The root helper: serves the app, resizes iOS with resize-apfs's engine, links the installer's engine |
| `../crates/inapp` | The installer's engine, in Rust: its state, the downloads and the install |
| `PROTOCOL.md` | How app and helper talk |
| `build.sh` | Builds app, helper and e2fsprogs into `out/device` |
| `build-e2fsprogs.sh` | Builds the e2fsprogs programs the helper runs (`ios`, or `host` for the tests) |
| `build-simulator.sh` | Builds only the app for the iOS simulator, to look at its screens (`--mock I1.2`) |
| `launch.sh` | Starts the installer on a connected device through Dopamine |

## Start it

```sh
./launch.sh
```

builds the installer when it is not built, signs and installs Dopamine as
resize-apfs's Dopamine route does (its checkout is used, `RESIZE_APFS` or
`../../resize-apfs-engine`), and after the jailbreak copies the installer to
the device over the USB cable and opens it. Typing `done` in the terminal
removes it again; never while a split or an install runs.

- `--jailbroken`: Dopamine is on the device and its jailbreak is active;
  nothing is signed or installed.
- `--allow-unsupported`: install on a board the release does not cover yet,
  with the kernel of its chip's page size. Such a system has no device tree
  and no boot entry; the boot partition stays empty.
- `--index URL`: another release index than the newest Omarchy A.

## What it does

The flow is one state machine (`crates/inapp/src/flow.rs`): welcome, split,
splitting, account, installing, installed or failed. Its screens are the
`I0` to `I3` frames of the installer's design.

- **Downloads** start as soon as the device is known to be supported, rest
  while iOS is resized and go on afterwards. They are resumed where an
  earlier run stopped.
- **The cache** is `/var/root/Library/Caches/orchard`, laid out as the
  tethered installer's (`files/<sha256>`, `board-data`, `firmware`, `store`).
  The root image is deleted once it is installed.
- **The install** takes all the space after iOS: Linux partitions there are
  erased, and a boot and a root partition are made by orchard's own plan
  (`orchard_boards::install_plan`). The root image goes onto its partition,
  then the root is finished as orchard's install system finishes it
  (firmware, account, time zone, kernel, boot entry). iOS's partition is
  never written.

iOS mounts neither ext4 nor a second FAT volume, so the root is changed with
e2fsprogs' own programs (`tune2fs`, `resize2fs`, `debugfs`, `e2fsck`), which
the helper brings along, and the boot filesystem is written whole
(`orchard_formats::fat32`). The kernel goes in as the root's package would
leave it: the package's files and its record in pacman's database, since
pacman itself cannot run from iOS.

## Checked, and not

`cargo test -p orchard-inapp` installs onto a disk kept in a file (it needs
`./build-e2fsprogs.sh host`, `mke2fs` and `zstd`).
`cargo run --release -p orchard-inapp --example drive -- DIR iPad7,5`
installs the published release the same way. The result of that was mounted
by a Linux kernel: pacman reads the kernel package's record, the password and
the PIN check out, and orchard's `boot-entries sync` finds nothing to change
on the boot partition.

Not run on a device yet: the whole route. In particular that iOS shows the
new partitions' nodes after the table is written, and that Dopamine lets the
helper start e2fsprogs' programs.

iOS 12.0 and 12.1 lack the Swift runtime the app and helper need; resize-apfs
bundles it for those, this does not yet. The app is registered from the
jailbreak root's Applications folder, which only the Dopamine route has.
