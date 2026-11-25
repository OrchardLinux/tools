# orchard-build

`build [DIRECTORY]` makes a project's RAM payload, or in a distribution
checkout its release; `inputs` resolves what a distribution's own build
takes from orchard finished. Both manage the cache and run producers: the
kernel, m1n1, the static tools and the images are made by orchard's recipe
scripts, run as commands, so a build can be redone by hand from what
`--explain` prints (`out/PLATFORM/commands.sh`). The crate assembles
nothing itself, so a payload's format is its producer's.

- **Recipes** come from `--data DIR`, else from a copy of orchard's newest
  nightly in the cache (`recipes/TAG`), fetched once and refreshed by `data
  update`. The nightly's source is the state its published components were
  built from, so a build from it finds them by key.
- **Projects**: `orchard.toml` says what a directory builds (`[payload]` or
  `[release]`), never for which board. A developer's choices are arguments
  or `orchard.local.toml`: device, platform, kernel tree, packages. Without
  them the board attached in DFU or Pongo mode picks the platform.
- **Components** are directories at `components/NAME/KEY` in the cache,
  never written after they are made. The key is the SHA-256 of the inputs
  that decide the content, written as orchard's Python writes JSON:
  orchard's CI publishes nightly components under keys computed that way,
  and a build takes the nightly's kernel, m1n1 or tools when its own key is
  the same (`ORCHARD_PUBLISHED=off` builds instead). Every file of an entry
  is recorded with its hash and checked before each use. Identical files
  are stored once, as hard links.
- **A miss says why**: entries keep their inputs (`key-inputs.json`), and a
  build that misses names the inputs that differ from the newest entry.
- **What ran** is kept with each entry (`commands.json`), so a build that
  reuses an entry still lists how it was made.
- **Kernel**: one per platform, in two layers. The base is the compiled
  kernel, every board's device tree, the in-tree modules and what external
  modules build against; a board's layer adds its external modules.
  Orchard's own source is the platform's upstream base with its patch
  series applied, kept as an editable tree in the cache
  (`kernel-work/PLATFORM-COMMIT`) with the compiler's output. Its identity
  is the Git tree its content would be committed as, so local edits are
  part of the key. A changed series is applied as the difference between
  the two trees, which keeps unchanged files' times and the compiler's
  objects; with local edits in the tree a series change is refused, never
  discarded. `--kernel-tree DIR` builds a developer's tree as it is, inside
  itself (`.orchard-out/`), cached apart.
- **A payload carries one board's external modules**: the chosen board's
  (`--device`, `orchard.local.toml`, or the attached one), else the
  platform's first.
- **Locks**: builds share the cache; `cache rm` and `cache prune` take it
  exclusively. One process builds an entry at a time and others wait for
  it. A kernel tree has its own lock, the one `kernel/build` takes.
- **Space**: a build refuses to start below 10 GB free
  (`ORCHARD_MIN_FREE_GB`) and first deletes least recently used entries
  until the build cache fits 20 GiB (`ORCHARD_CACHE_BUDGET_GB`): older
  components and work directories and kernel trees without local edits,
  never the newest of each.
- **Output**: producers print to stderr; the result is one document on
  stdout. A failed producer gives the error code `build`, with a hint when
  Podman is missing or its VM is not running.
- **Python**: the producers need 3.11: `ORCHARD_PYTHON`, else the newest
  `python3.N` on PATH.
- **The USB session** (`transport`, on by default): a payload gets orchard's
  session helper and this computer's key for RAM payloads, made once in the
  cache (`private/ram-ssh`).
