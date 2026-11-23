# Companion protocol

How the installer on the computer talks to the companion's helper on an
iPad booted into iPadOS with the companion overlay. Version 1.

## Transport

The helper listens on TCP `127.0.0.1:49720` on the iPad, never on a network
interface. The installer reaches it through the computer's usbmuxd (`Connect`
to port 49720 of the device, by UDID). One connection at a time; a second one
is refused while the first lasts.

Messages are JSON objects, one per line (UTF-8, `\n`), at most 1 MiB each.
The installer sends one request and reads its answer before the next.

## Authentication

The overlay carries a 32-byte secret the installer made for this boot,
readable by root only (`/cores/binpack/usr/share/orchard-companion/token`, 64
hex digits). The first request must be `hello` with it; anything else, or a
wrong token, closes the connection.

## Requests

```json
{"hello": {"token": "…", "protocol": 1}}
{"snapshot": {}}
{"split": {"apfs_bytes": 64000000000, "revision": "…", "replacing": ["…"]}}
{"ping": {}}
{"log": {}}
```

Answers are `{"ok": …}` or `{"error": {"message": "…"}}`.

- `hello` → `{"protocol": 1, "ecid": "1a2b3c", "machine": "iPad7,5"}`. `ecid` is
  the device's own (`IODeviceTree:/chosen` `unique-chip-id`), lowercase hex
  without padding.
- `snapshot` → `{"layout": …, "operation": … | null}`: resize-apfs's
  `StorageLayout` and `Operation` as its engine reports them (dates as
  seconds since 2001, Swift's default). `layout.revision` names this exact
  layout.
- `split` → the operation started, as in `snapshot`. Shrinks the APFS
  container to `apfs_bytes` and leaves the space after it unallocated; the
  installer's plan makes the Linux partitions later. Refused unless
  `revision` is the current layout's, and while an operation runs.
  `replacing`: the ids of all partitions after APFS, which the user
  approved erasing on the split screen. It must name exactly all of them;
  the split removes their GPT entries in its verified GPT write, so it
  always leaves iPadOS and free space after it (resize-apfs's plan).
- `log` → `{"text": "…"}`: the last 64 KiB of the helper's log
  (`/cores/orchard-companion.log`), with seconds since the helper started.
  The installer saves it after connecting, to see where the time between
  boot and the app goes.
- `ping` → `{}`. The installer pings at least every 2 seconds; the app shows
  the computer as connected while pings arrive.

## Status for the app

The helper writes `/var/run/orchard-companion/status.json` (root's, mode
0644) whenever something changes and every second while connected:

```json
{"host": true, "phase": "idle", "started": null, "message": null}
```

`phase` is `idle`, `splitting`, `done` or `failed`; `started` is when the
split began (Unix seconds). The app only reads this file.
