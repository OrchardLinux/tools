# This computer: doctor and setup

orchard2 fetches its own tools (palera1n, checkra1n, m1n1, Pongo) when a run
needs them and talks to USB by itself, without libusb. What it takes from
the computer is little, and `doctor` lists it with what each is for.

## doctor

```sh
orchard2 doctor
```

changes nothing. It lists what orchard2 takes from the computer under what
it is for, each with what it does there:

```
This computer

  For building
    ✓ Python       python3.14
    ✓ git
    ! Podman       its VM is stopped
                   The recipes compile in its containers.
                   orchard2 setup podman
    ✓ Free space   33 GB

  For signing in over SSH
    ✓ ssh-keygen

1 thing to get:  orchard2 setup podman
```

What is there takes one line. What is not says what is wrong, why it is
needed, and the command that gets it: `!` when orchard2 can, `✕` when only
you can. The last line is one command for everything orchard2 can get.
In `--json`
these are the states `ready`, `missing` and `yours`.

| Name | Is | Needed for |
| --- | --- | --- |
| `usb` (Linux) | a udev rule that lets your account open the device on USB in every stage, m1n1 and the serial consoles included | every command that works on a device |
| `python` | Python 3.11 or newer | `build`, `inputs`: orchard's recipes are Python scripts |
| `git` | git | `build`, `inputs`: fetching the kernel's source, and a checkout's state |
| `podman` | Podman, and on macOS its VM running | `build`, `inputs`: the recipes compile in containers |
| `ssh` | `ssh-keygen` | `install --ssh`, and a RAM payload with the USB session |
| `disk` | 10 GB free where the cache is | `build` |

Someone who only installs and starts systems needs none of the build's
four. On macOS nothing is needed for USB.

`--json` gives `{"ready": …, "checks": [{"name", "state", "needed_for",
"detail", "setup"}]}`.

## setup

```sh
orchard2 setup podman
orchard2 setup usb git
```

gets the named things, one after the other, and prints each command before
it runs it. Without a name it only shows what `doctor` shows.

| Name | Does |
| --- | --- |
| `usb` | installs the udev rule (`/etc/udev/rules.d/70-orchard-installer.rules`) and has udev apply it; asks for your password (a window in a desktop session, else `sudo`). Reconnect a device that is plugged in |
| `python`, `git`, `podman` | installs the package: with glu, else Homebrew, on macOS; with apt, dnf or pacman through `sudo` on Linux |
| `podman`, on macOS | also creates its VM if there is none, and starts it |

What is ready is left alone. `ssh` and `disk` are yours: orchard2 says what
is missing and stops (`unsupported`). With no package manager it knows, a
tool is yours too.

`install` and `launch` install the USB rule by themselves when access is
refused, so `setup usb` is for the other commands, or for doing it ahead.
