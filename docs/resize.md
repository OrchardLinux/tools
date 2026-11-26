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
are its short forms. This version runs `palera1n`, which is also taken when
none is named; for the other two, resize-apfs's own release has
`./resize-apfs boot`.

DFU mode does not tell the iOS version, so a device on iOS 14 or older is
only refused by palera1n itself.

## Options

- `--wait SECONDS` waits for the device in DFU mode.
- `--timeout SECONDS` (300) is how long iPadOS may take to start.

`install --split` resizes without the app: the computer shows the storage
and asks ([install.md](install.md)).
