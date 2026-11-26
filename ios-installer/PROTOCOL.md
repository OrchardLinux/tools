# In-app installer protocol

How the installer app (runs as `mobile`) talks to its root helper on the same
device. Version 1.

## Transport

The helper listens on the Unix socket `helper.sock` in its socket folder
(root's, group `mobile`, mode 0660). The first request on a connection carries
the token from `client.token` there (64 hex digits, mode 0640, group
`mobile`), which the helper makes anew each time it starts.

The folder is `/private/var/OrchardInstaller/run` for the app inside Dopamine
(`--ipc-dir`; the app opens it as root), else `/var/run/orchard-installer`.
The helper answers root, and the app at `--ui-path` where it was started with
one.

A message is a 4-byte big-endian length and that many bytes of UTF-8 JSON, at
most 1 MiB. The app sends one request and reads its answer before the next.

## Requests

A request is an object with `token` and exactly one more key, the request's
name, whose value is its arguments:

```json
{"token": "…", "view": {}}
{"token": "…", "split": {"ios_bytes": 96000000000}}
{"token": "…", "keep": {}}
{"token": "…", "check": {}}
{"token": "…", "continue": {}}
{"token": "…", "install": {"password": "…", "pin": "123456"}}
{"token": "…", "retry": {}}
{"token": "…", "log": {}}
```

Every answer is `{"ok": VIEW}` or `{"error": {"message": "…"}}`, except
`log`, which answers `{"ok": {"text": "…"}}` (the end of the helper's log).
An error changes nothing; the app shows it where the request was made.

- `view`: nothing changes. The app asks once a second.
- `split`: shrink iOS (or grow it) so that its partition has `ios_bytes`, and
  erase every partition after it. Only on the `split` screen. The user has
  ticked the backup box and, where partitions are erased, confirmed the dialog.
- `keep`: go on with the storage as it is. Only on the `split` screen while
  `split.fits` is true.
- `check`: read the storage again (after the user freed space).
- `continue`: from `welcome` to `split`, once `welcome.checked` is true, and
  from `split_done` to `account`.
- `install`: start the install with the account's password (8 characters or
  more) and lock screen PIN (6 digits). Only on the `account` screen while
  `account.download` is `ready`. Where `account.erases` is not empty the user
  has confirmed the dialog.
- `retry`: from `failed` back to where the install can be started again.

## The view

Everything the app shows comes from the view; the app keeps only what the
user is typing or dragging.

```json
{
  "screen": "split",
  "system": "Omarchy A",
  "supported": true,
  "device": {"name": "iPhone XR", "kind": "iPhone", "os": "iOS"},
  "welcome": {"checked": true},
  "storage": {
    "total_bytes": 128000000000,
    "ios_bytes": 128000000000,
    "ios_used_bytes": 41000000000,
    "outside_bytes": 0,
    "outside": [{"name": "BOOT", "filesystem": "FAT32", "bytes": 1073741824}]
  },
  "split": {
    "min_ios_bytes": 45000000000,
    "max_ios_bytes": 116000000000,
    "suggested_ios_bytes": 96000000000,
    "needed_bytes": 12000000000,
    "fits": false,
    "refused": null
  },
  "splitting": {
    "target_ios_bytes": 96000000000,
    "steps": [{"title": "Check storage", "state": "done"},
              {"title": "Shrink iOS", "state": "active"},
              {"title": "Check the result", "state": "pending"}],
    "confirmed_seconds_ago": 4,
    "elapsed_seconds": 720,
    "reconnecting": false
  },
  "account": {"user": "tablet", "download": "downloading",
              "erases": [{"name": "BOOT", "filesystem": "FAT32", "bytes": 1073741824}]},
  "installing": {"phase": "writing", "percent": 64},
  "installed": {"seconds": 60.4},
  "failed": {"message": "Something went wrong while writing it. iOS and your data weren't touched.",
             "detail": "writing the system: no space left on device"}
}
```

`screen` is one of `welcome`, `unsupported`, `split`,
`splitting`, `split_done`, `account`, `installing`, `installed`, `failed`.
`system` and `device` are always there; `storage` is `null` until the storage
has been read, and `supported` and `device.name` are `null` until the release
and the board data have been read. Of the other objects only the one the
screen needs is set, the rest are `null`.

- `welcome.checked` says the storage, the release and the board are known;
  until then the app's Continue is dimmed.

- `device.kind` is `iPhone`, `iPad` or `iPod touch`; `device.os` is `iOS` or
  `iPadOS`.
- `storage.outside` are the partitions after iOS, which the system replaces;
  `outside_bytes` is all the space after iOS, free space included.
- `split.min_ios_bytes` is the least iOS can shrink to (null when iOS cannot
  say), `max_ios_bytes` the most it can keep so that the system still fits,
  `needed_bytes` the least the system needs. When `max_ios_bytes` is below
  `min_ios_bytes` there is not enough free space. `fits` says the space after
  iOS is already enough. `refused` is why the last split did not happen.
- `splitting.steps[].state` is `done`, `active` or `pending`;
  `confirmed_seconds_ago` is null before the first confirmation;
  `reconnecting` is true while the helper has not heard from the split for 15
  seconds.
- `account.download` is `downloading`, `ready` or `offline`.
- `installing.phase` is `writing` (with `percent`) or `finishing`.
