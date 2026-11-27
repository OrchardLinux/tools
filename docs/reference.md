# Reference

## Options every command takes

| Option | Is |
| --- | --- |
| `--json` | one JSON document on stdout instead of text |
| `--data DIR` | the orchard data (`ORCHARD_DATA`) |
| `--cache DIR` | the cache (`ORCHARD_CACHE`) |
| `--ecid ECID` | the device, by its ECID in any hex spelling |
| `--location LOCATION` | the device, by its USB location as `devices` prints it |

## Output

Text for people and progress go to stderr where a command also has a
result; the result goes to stdout. With `--json` stdout carries exactly one
document, or for `install` and `launch` one event per line. Colour is used
on a terminal only, and not with `NO_COLOR` set.

### Errors

```
error: MESSAGE
HINT
```

on stderr, or with `--json` on stdout:

```json
{"error": {"code": "locked", "message": "…", "hint": "…"}}
```

| Code | Exit status | Means |
| --- | --- | --- |
| `invalid-input` | 2 | wrong use, or input that cannot be right |
| `no-device` | 3 | no matching device is connected |
| `ambiguous` | 3 | several things match and one was needed |
| `usb` | 4 | talking to the device failed |
| `format` | 5 | a file is not what it should be |
| `network` | 6 | a download failed |
| `io` | 7 | reading or writing a file failed |
| `not-found` | 8 | something named is not there |
| `too-large` | 9 | something is larger than it may be |
| `unsupported` | 10 | this version, or this device, cannot do it |
| `timeout` | 11 | it took too long |
| `locked` | 12 | another orchard holds the device |
| `device-changed` | 13 | the device is not the one, or not in the state, it was |
| `refused` | 14 | the device or a check refused |
| `build` | 15 | a build's recipe failed |

A `build` or `inputs` that Ctrl-C ended exits with 130. `session` exits
with its command's status.

### install and launch

They have their own statuses:

| Status | Means |
| --- | --- |
| 0 | done |
| 2 | wrong use |
| 3 | no device in DFU mode within `--wait` |
| 4 | the device is not supported |
| 5 | an answer is needed and there is no terminal |
| 6 | it failed, or files it got for the device can't be used |
| 7 | a download failed: the network, or the release's own files |
| 8 | interrupted |

and with `--json` these events, one object per line with an `event` field:

| Event | Carries |
| --- | --- |
| `release` | `name`, `version` |
| `state` | `state`: `waiting-for-dfu`, `found`, `unsupported`, `several-in-dfu`, `usb-access-needed`, `waiting-for-ipados`, `choose-split`, `splitting`, `split-done`, `dfu-again`, `create-account`, `confirm-erase`, `not-prepared`, `installing`, `install-failed`, `starting`, `launching`, `choose-system`, `nothing-installed` |
| `needs` | `action`: `dfu` (`device`, `mode`, `steps`, `wait_s`), `input` (`flag`), `disconnect-others`, `connect-named` and `wait-for-device` (`devices`), `replug` |
| `approved` | `by`: the option that answered |
| `split` | the storage: total, iPadOS and what it uses, the suggested, smallest and largest size, the partitions after iPadOS |
| `recovery` | the guided run from recovery mode into DFU mode, once per phase: `phase` (`ready`, `get-ready`, `hold`, `restarted`, `let-go`, `waiting`), `device`, `seconds` (how long the phase runs, or null), `again` and `problem` (in `ready`: the last attempt came back in recovery mode; why the device couldn't be reached) |
| `stage`, `retry` | where a start is, and that it is tried again |
| `progress` | `percent`, or `step` |
| `systems` | `entries`: `id`, `title`, `kernel`, `default` (launch) |
| `done` | `installed_in` and `ssh`, or `started` |
| `error` | `code`, `message`, `exit`, `log` |

## Environment

### Everywhere

| Variable | Is |
| --- | --- |
| `ORCHARD_DATA` | `--data` |
| `ORCHARD_CACHE` | `--cache` |
| `NO_COLOR` | no colour |

### install and launch

Switches for trying a variant without a new build; each one set is written
to the run's log.

| Variable | Does |
| --- | --- |
| `ORCHARD_BOOT=ROUTE` | the boot route, as `--boot ROUTE` ([install.md](install.md#how-the-device-is-entered)); `--boot` counts over it |
| `ORCHARD_PALERA1N_BETA=0` | palera1n v2.4 instead of the 3.0 beta |
| `ORCHARD_PORT_RESET_SECONDS=N` | seconds after the exploit before the USB port is reset (Linux, 10 by default); 0: no reset |
| `ORCHARD_BUSY_CPU=1` | as `--busy-cpu` ([install.md](install.md#when-the-exploit-never-gets-in)) |
| `ORCHARD_KEEP_AWAKE=1` | keeps the CPU out of deep sleep while the exploit runs (Linux) |

### build and inputs

| Variable | Does |
| --- | --- |
| `ORCHARD_PYTHON` | the Python that runs the recipes (3.11 or newer) |
| `ORCHARD_PUBLISHED=off` | builds every component here instead of taking the nightly's |
| `ORCHARD_KERNEL=local` | builds the kernels here |
| `ORCHARD_KERNEL_TREE` | `--kernel` |
| `ORCHARD_CACHE_BUDGET_GB` | the build cache's limit (20; 0: none) |
| `ORCHARD_MIN_FREE_GB` | free space a build needs to start (10; 0: no check) |
| `ORCHARD_BUILD_NATIVE=1` | the recipes build on this host instead of in Podman (CI builders); part of the kernel's key with `ORCHARD_BUILD_ENV_ID` |
| `ORCHARD_KERNEL_JOBS`, `ORCHARD_CCACHE` | read by the kernel recipe |

A distribution's build script is given `ORCHARD_CHECKOUT`, `ORCHARD_TOOL`,
`ORCHARD_PYTHON`, `ORCHARD_CACHE`, `ORCHARD_KERNEL_TREE` and
`ORCHARD_PACKAGES` ([build.md](build.md#a-distributions-release)).

### Boot commands and firmware

| Variable | Does |
| --- | --- |
| `ORCHARD_FIRMWARE_KEYS` | `--keys` |
| `ORCHARD_SESSION_FD`, `ORCHARD_SESSION_LOCATION`, `ORCHARD_SESSION_STATE_FD` | set by `session` for the commands inside it; not for use by hand |
