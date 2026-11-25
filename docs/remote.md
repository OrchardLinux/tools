# A running system: shell, exec, push, pull

Four commands work on a system that runs on the device: a RAM payload, or an
installed system. They start nothing and change no boot state.

```sh
orchard2 shell                       # a shell on it, at this terminal
orchard2 exec -- uname -a            # one command; its exit status is this command's
orchard2 push build/app /root/app    # a file to it
orchard2 pull /var/log/messages .log # a file from it
```

## Which system

The one running RAM payload on USB; with none, the one installed system.
Two of a kind is `ambiguous`: connect one, or name an address with `--host`.

## How it is reached

| System | Signed in as | With |
| --- | --- | --- |
| a RAM payload | root | this computer's key for payloads, which `orchard2 build` put into it |
| an installed system | its user | this computer's key, which `orchard2 install --ssh` imported |

- **A payload's host key** is new each time it starts. It is read over the
  USB serial console, which is the cable and so trusted, and pinned for that
  start: nothing else on the network can stand in for the device.
- **An installed system's host key** is learned on the first connection and
  kept under the unit's own name, which the system says on USB: every
  device on the cable has the same address, and two tablets are still two
  hosts. `orchard2 install --ssh` forgets the unit's old key, since the new
  system makes a new one, and remembers the user per unit. `--user NAME` signs in as another user than the one the install set
  up; root is `sudo` there, as on any system.
- **`--host ADDRESS`** reaches a system over a network instead of the cable,
  as an installed system; with `--ram`, as a payload and root. Its host key
  is trusted on first use, as `ssh` does: check it some other way the first
  time.

A payload built with `transport = false`, or one whose init does not run
`/usr/libexec/orchard/usb-session`, has no SSH; `push` and `pull` then take
`--serial`.

A device that refuses the key says so, with what gives it the key. Keys
that another tool made are not tried.

## Files

`push LOCAL REMOTE` writes the file beside its place (`REMOTE.part`), has
the device compute its SHA-256, and moves it into place only when that is
the file's. `pull REMOTE LOCAL` does the same the other way round. A
checksum that differs leaves the `.part` file and fails.

- Over SSH, any size. A connection that drops is made again once, and the
  file starts over.
- `--serial`: over the USB serial console, for a payload without SSH, 1 byte
  to 16 MiB. A place that cannot be written is refused before any byte of
  the file is sent. Not for an installed system, whose console is a login.

`--json` gives `{"from", "to", "size", "sha256"}`.

## Where the keys are

In the cache, readable by you only: `private/ram-ssh/` (the key for
payloads and the pinned host key of the payload that runs) and
`private/ssh/` (the key for installed systems, their host keys, and the
user each install with `--ssh` set up: `user-UNIT` per unit, `user` for the
last one).
