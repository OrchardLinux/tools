# cache

```sh
orchard2 cache
```

lists what is cached, with sizes and when each was last used:

- **downloaded files**: firmware, release files, tools;
- **build components** (`components/NAME/KEY`): kernels, m1n1, Pongo, the
  static tools, each under the key of what it was made from;
- **work directories** of the recipes, which make a rebuild quick;
- **kernel trees**, with whether they hold local edits.

The last line is the build cache's size and its limit.

| Command | Does |
| --- | --- |
| `cache` or `cache ls` | lists |
| `cache rm COMPONENT[/KEY]…` | deletes a component's entries, or the one whose key starts with KEY |
| `cache prune [--keep N]` | keeps the N most recently used entries of each component and work directory (5), deletes what unfinished builds left, and on macOS gives the Podman VM's freed space back |
| `cache clear` | deletes the downloaded files and firmware keys; they are fetched again when needed |

`rm` and `prune` wait for running builds to finish. Kernel trees are never
deleted by `prune`; a build deletes the least recently used ones without
local edits when the cache is over its limit, never the newest
([build.md](build.md#space)).

## Where it is

`--cache DIR`, else `ORCHARD_CACHE`, else `~/Library/Caches/orchard2` on
macOS and `${XDG_CACHE_HOME:-~/.cache}/orchard2` on Linux.

| Inside | Holds |
| --- | --- |
| `files/` | downloads, by SHA-256 |
| `components/`, `work/`, `kernel-work/`, `git/`, `ccache/` | the build's ([build.md](build.md)) |
| `recipes/` | the build's copy of orchard's nightly recipes |
| `data/`, `board-data/` | board data |
| `tools/` | palera1n and checkra1n, unpacked |
| `firmware/`, `firmware-keys/`, `store/` | built firmware, keys, and Apple firmware it is built from |
| `private/` | this computer's keys for SSH into a system it installed or built |
| `logs/` | run logs |

The limit of the build's part is 20 GiB (`ORCHARD_CACHE_BUDGET_GB`; 0: none).
