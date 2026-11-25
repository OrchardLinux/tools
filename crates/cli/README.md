# orchard2

The command line. `install` and `launch` are a second front end of the
installer's and the launcher's flow (`crates/flow`, run by `crates/runtime`):
what the windows show, they say; what the windows ask, they ask or take from
an option. `build`, `inputs` and the build part of `cache` are described in
[`crates/build`](../build/README.md).

Every command takes `--json`, `--data DIR` (`ORCHARD_DATA`: board data; for
`build` the recipes too), `--cache DIR` (`ORCHARD_CACHE`), and `--ecid` or
`--location` to name one device.

## install

`orchard2 install [SOURCE]` installs a distribution on the iPad or iPhone
that is put in DFU mode.

- **SOURCE** is a name (`omarchy-a`: the newest release of
  `OrchardLinux/NAME`), the URL of a release index or of one release's
  manifest, or such a file. `--index` takes the same. Without one, in a
  distribution checkout, it is the release its last `build` made.
- **Where it goes**: into the space after iPadOS. Partitions there are erased
  after a yes (`--yes`). `--split [GB]` resizes iPadOS first; with a terminal
  and no `--split` it asks.
- **The account**: the password and the lock screen PIN are asked, or read
  from `--password-file` and `--pin-file` (`-`: the next line of stdin).
  `--ssh` lets this computer sign in over SSH, `--no-ssh` doesn't ask.
- **Without a terminal** nothing is asked: what an option does not answer
  ends the run with exit status 5 and the name of the option.
- A 32-bit board, and one whose data says it is not supported, is refused
  before anything is started on it.

## launch

`orchard2 launch` starts a system installed on the device.

- With one system installed it starts it. With several it shows them and the
  one under the mark starts on Enter: ↑ and ↓ (or j and k) move the mark, a
  number starts that row, Esc or q leaves. Without a terminal the default
  starts.
- `--list` prints the systems and starts nothing; `--entry ID` starts that
  one.

## Waiting for DFU mode

Both commands wait for a device in DFU mode and show the steps for it.

- **Nobody has seen the device yet**: the steps stand under three tabs, as
  in the windows: Home button, No Home button, iPhone 7 and 7 Plus. ← and →
  change the tab (n, 7 and h name one); t shows tips.
- **The device is connected in another mode** (running iPadOS, recovery
  mode, the install system): its name, that mode, and the steps for its own
  buttons.
- **More than one iPhone or iPad is connected**: nothing goes on until only
  one is.
- `--wait SECONDS` ends the run with exit status 3 when no device shows up in
  DFU mode in that time.

## One device

`--ecid` and `--location` (as `orchard2 devices` prints them) name the device
a run is for. No other device is found, started or installed on; while only
others are connected the run says that the named one is missing. With the
named device and another connected nothing goes on, as without them: the
tool that enters the device cannot be pointed at one of two.

## How the device is entered

From DFU mode palera1n runs the boot ROM exploit and starts Pongo, on every
board; checkra1n does with `ORCHARD_ENTRY=checkra1n`. Both are pinned by size
and SHA-256 and fetched into the cache.

Switches from the environment, for trying a variant without a new build
(each one set is written to the log):

| Variable | What it does |
| --- | --- |
| `ORCHARD_ENTRY=palera1n` or `checkra1n` | that tool enters the device; palera1n without it |
| `ORCHARD_PALERA1N_BETA=0` | palera1n v2.4 instead of the 3.0 beta |
| `ORCHARD_PORT_RESET_SECONDS=N` | seconds before the USB port reset after the exploit (Linux); 0: none |
| `ORCHARD_KEEP_AWAKE=1` | keeps the CPU out of deep sleep while the exploit runs (Linux) |

Which entry and chain to use is otherwise the boot commands' to offer
(`prep`, `load`, `boot`); `install` and `launch` take no option for it.

## Output and exit status

Words go to the terminal (stderr), the run's log to `--log FILE`, else to
`logs/` in the cache. With `--json`, stdout carries one event per line:
`release`, `state`, `needs` (`dfu` with the device, its mode and the steps;
`input` with the option that is missing; `disconnect-others`;
`connect-named`; `replug`), `approved`, `split`, `stage`, `retry`,
`progress`, `systems`, `done`, `error`.

| Status | Meaning |
| --- | --- |
| 0 | done |
| 2 | wrong use |
| 3 | no device in DFU mode within `--wait` |
| 4 | the device is not supported |
| 5 | an answer is needed and there is no terminal |
| 6 | it failed |
| 7 | a download or a release file failed |
| 8 | interrupted |

Ctrl-C at a question ends the run at once. While the storage is split or the
system is written, the first Ctrl-C only warns.

## cache

`orchard2 cache` lists what is cached: downloaded files, and the build's
components, work directories and kernel trees with their sizes and last use.

- `cache rm COMPONENT[/KEY]…` deletes build components.
- `cache prune [--keep N]` keeps the N most recently used entries of each
  component (5).
- `cache clear` deletes the downloaded files.

The cache is `~/Library/Caches/orchard2` on macOS and
`${XDG_CACHE_HOME:-~/.cache}/orchard2` on Linux.
