# orchard2

`orchard2` is the command line of the Orchard tools: it builds what runs on
an iPad or iPhone, puts it there over USB and starts it. The graphical
installer and the launcher are built on the same libraries; `install` and
`launch` are their flow on a terminal.

| To | Read |
| --- | --- |
| install a distribution on a device | [install.md](install.md) |
| start an installed system | [launch.md](launch.md) |
| build a RAM payload or a distribution's release | [build.md](build.md) |
| open a shell on a running system, run a command, copy a file, allow SSH afterwards, save Apple's device tree | [remote.md](remote.md) |
| see or clean what is cached | [cache.md](cache.md) |
| list devices, name one, keep one for several commands | [devices.md](devices.md) |
| take a device through a boot chain by hand | [boot-commands.md](boot-commands.md) |
| talk to DFU mode, iBoot, Pongo or m1n1 directly | [low-level.md](low-level.md) |
| look at board data and firmware | [data-and-firmware.md](data-and-firmware.md) |
| see what this computer lacks, and get it | [host.md](host.md) |
| script it: options, environment, JSON, exit statuses | [reference.md](reference.md) |

## What every command shares

- `--json` prints one JSON document on stdout instead of text (`install` and
  `launch`: one event per line). Progress and what tools print go to stderr.
- `--data DIR` (`ORCHARD_DATA`) is the orchard data: board descriptions, and
  for `build` the recipes. Without it each command uses a copy it keeps in
  the cache ([data-and-firmware.md](data-and-firmware.md),
  [build.md](build.md#recipes)).
- `--cache DIR` (`ORCHARD_CACHE`) is the cache
  ([cache.md](cache.md#where-it-is)).
- `--ecid` and `--location` name one device ([devices.md](devices.md)).

No command touches a device unless it says so: `build`, `inputs`, `cache`,
`doctor`, `boards`, `board`, `data`, `fw` (except `fw build --device-keys`),
`m1n1 pack` and every `--plan` work without one.
