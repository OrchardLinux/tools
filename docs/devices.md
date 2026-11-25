# Devices

## devices

```sh
orchard2 devices [--mode MODE]
```

lists the iPhones and iPads on USB and takes no lock, so it works while
another command holds a device.

| Mode | The device |
| --- | --- |
| `dfu` | is in DFU mode (the boot ROM) |
| `recovery` | is in recovery mode (iBoot) |
| `normal` | runs iOS or iPadOS |
| `pongo` | runs PongoOS |
| `m1n1` | runs m1n1, waiting in its proxy |
| `linux` | runs Linux |

A Linux device that is one of Orchard's own also says which **system** it
is: `recovery`, `ram` (started in RAM), `install` (the installer's system)
or `installed`.

The columns are the mode, the system, the USB **location**, the chip
(CPID), board number (BDID) and ECID where the device reports them, the
tool that pwned a boot ROM, the board's name from the orchard data, the
serial port (a Linux system's console, or m1n1's proxy) and the product
string. A column shows only when a listed device has something in it.
`--json` gives `{"devices": […]}` with the same as fields. The board name
comes from data already on disk: `devices` downloads nothing.

A device that runs iOS or iPadOS says only "iPhone" or "iPad" on USB. The
listing asks each one what it is and for its ECID, all at once, and waits
0.3 s for the answers: one that answers is shown by its name ("iPad (6th
generation)") with its board and ECID, so its ECID can go into `--ecid`
before it is ever in DFU mode. One that has just been plugged in may not
answer in time; it is shown as USB shows it, and the next listing knows it.
Asking reads two values and changes nothing on the device. `--json` has the
name as `name` and the product type (`iPad7,5`) as `model`.

`--mode` lists only `dfu`, `recovery`, `pongo`, `m1n1` or `linux`.

## Naming a device

A command that works on a device uses the one that is connected. With
several:

- `--ecid ECID`, in any hex spelling;
- `--location LOCATION`, exactly as `devices` prints it (macOS: `0x…`;
  Linux: `BUS-PORT.PORT`). The location stays the same while a device
  changes modes on the same cable.

None connected is the error `no-device`; several without a name is
`ambiguous` (both exit status 3). `prep`, `load` and `boot` pass over a
device that already runs Linux when exactly one other is there.

### One device for install and launch

`install` and `launch` take the same two options, with one difference: the
tool that enters a device from DFU mode cannot be pointed at one of two. So
they find, start and install on no device but the named one, say that it is
missing while only others are connected, and still wait until every other
iPhone and iPad is unplugged.

## wait

```sh
orchard2 wait MODE [--timeout SECONDS]
```

waits until one device shows up in `dfu`, `recovery`, `pongo`, `m1n1` or
`linux` (30 s by default) and prints it as `devices` does. Several at once
is `ambiguous`; none in time is `timeout` (exit status 11).

## session

```sh
orchard2 session [--timeout SECONDS] -- COMMAND…
```

keeps one device for a whole script. It picks the device, locks it, runs
COMMAND and ends with its exit status. Every `orchard2` inside works on
that device without naming it, also after it changed modes, and they share
what was learned about it: its identity, and a state USB cannot show (see
[boot-commands.md](boot-commands.md#states)).

```sh
orchard2 session -- sh -c '
    orchard2 prep &&
    orchard2 load linux.bin --wait linux'
```

- `--timeout` is the time all device work in the session may take together;
  without it there is none. A command's own `--timeout` counts too, and the
  shorter one ends it.
- **The lock** is per USB location (`/tmp/orchard2-locks/`). While a device
  is locked, another `orchard2` that wants it fails at once with `locked`
  (exit status 12) and says which process holds it. The boot commands,
  the low-level commands and `run` take the lock for as long as they run.
- `install`, `launch`, the installer and the launcher take the same lock
  for a device once it is in DFU mode, which is where their work on it
  starts, and keep it until they are done with it, through every mode it
  goes through. A window that is only open holds nothing. A device in DFU
  mode that other work holds is left to it: they say "This device is in
  use", name the program that works on it, and go on when it is free.
  They wait where the boot commands fail, because a person is watching.

What a session also keeps, the device's identity and a state USB cannot
show, `install` and `launch` do not share with other commands yet
([TODO.md](../TODO.md)).
