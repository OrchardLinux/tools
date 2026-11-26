# resize

```sh
orchard2 resize
```

gets **Resize APFS** onto the iPad or iPhone in DFU mode and running. Resize
APFS is the app that shrinks iPadOS to make room for Linux, and grows it
back; it shows the storage and asks before it changes anything. The person
does the rest in the app, on the device. Nothing is installed for good: the
app is gone after the next restart.

1. The newest release of
   [OrchardLinux/resize-apfs](https://github.com/OrchardLinux/resize-apfs)
   is fetched and checked against the checksum it is published with, once
   per release.
2. The device is taken from DFU mode into iPadOS with the app in it.
3. Unlock the device: Resize APFS appears on the Home Screen within a
   minute.

Back up the device first.

## The access route

An **access route** is the way an app of ours gets to run inside iPadOS
with access to the storage. Which routes a device has follows from its chip
and the iOS version it runs, by `routes.toml` in the board data.

| Route | Is | Works on |
| --- | --- | --- |
| `palera1n` | palera1n starts iPadOS from DFU mode with the app in the place of its own loader app | A8 to A11, iOS 15 or later |
| `checkra1n` | checkra1n starts iOS and the app is copied in over the cable | A7 to A11, iOS 12 to 14 |
| `dopamine` | the app is installed and gets access by itself, without DFU mode | A8 to A13 and later, by iOS version |

`--access ROUTE` names one; `--palera1n`, `--checkra1n` and `--dopamine`
are its short forms. palera1n is taken when none is named.

### checkra1n

On iOS 12 to 14 nothing can be put into the start itself, so the app is
copied onto the device once iOS runs, and taken off again at the end:

1. checkra1n starts iOS from DFU mode. Unlock the device.
2. The app and its helper are copied over the cable, and the app appears on
   the Home Screen.
3. Resize in the app. When you are done, type `done` in the terminal: the
   helper is stopped and the app removed. While a resize still runs,
   nothing is stopped; you are asked again.

The run needs a terminal and stays connected until then. A session that
ended before its cleanup is undone at the start of the next one. It uses
the computer's own `ssh` program.

DFU mode does not tell the iOS version, so a device on iOS 14 or older is
only refused by palera1n itself.

### dopamine

Without DFU mode. The app is installed on the device and stays there; it
gets storage access by itself when it is opened.

1. Turn on Developer Mode: Settings > Privacy & Security > Developer Mode.
   The device restarts; confirm.
2. Connect the device, unlock it and trust the computer.
3. `orchard2 resize --dopamine` puts Resize APFS into the pinned official
   Dopamine app, signs it with your Apple ID and installs it. On Linux, and
   on a Mac whose Xcode has no account, xtool signs: it asks for the Apple
   ID, its password and the two-factor code in the terminal the first time
   (they go to Apple only) and keeps the session. On a Mac whose Xcode is
   signed in, that account signs, with nothing to sign in to; `--team ID`
   names one of several teams, and `--xtool` takes xtool all the same.
4. On the device, trust your Apple ID: Settings > General > VPN & Device
   Management.
5. Open Resize APFS and tap Continue. The screen restarts once; unlock the
   device when it comes back.

A free Apple ID signs the app for 7 days; run the command again when it
stops opening. Signing happens on your computer, with your account, for
your device.

A device that was started through DFU never opens this installed copy by
itself: its helper stays idle there.

## Options

- `--wait SECONDS` waits for the device in DFU mode.
- `--timeout SECONDS` (300) is how long iPadOS may take to start.

`install --split` resizes without the app: the computer shows the storage
and asks ([install.md](install.md)).
