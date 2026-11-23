# Vendored firmware tools

Unmodified upstream sources. `build.rs` compiles the patchers to executables
and embeds them; `orchard_boards::boot_firmware::patcher` runs each in its own child
process. Each source keeps its licence text beside it.

| Source | Commit | Licence |
| --- | --- | --- |
| [hBootPatcher](https://github.com/HoolockLinux/hBootPatcher) | `981d35bced1e73a18cd1921ed214c2e57020ac8a` | MIT |
| [plooshfinder](https://github.com/plooshi/plooshfinder) (inside hBootPatcher) | `c151549ddc65e34222348a2bcfdc266f9aeda3d6` | LGPL-3.0 |
| [iBoot32Patcher](https://github.com/dora2ios/iBoot32Patcher) | `77ab155cb418d67f688e2e85f3e83c77a2431660` | GPL-3.0 |

The chip IM4M manifests are Apple-signed and so neither in this repository nor
in the binaries: an IMG4 chain downloads its chip's from
[remote_boot](https://github.com/HoolockLinux/remote_boot)
`e254a58b7d7c68c9e34a407a0beb0aab4861a6b6` (BSD-3-Clause) the first time it is
built, checked against the SHA-256 pinned in
`crates/boards/src/boot_firmware.rs`, into the cache. The installer's palera1n
route never needs them. The zero-entry trust cache wrapped into the IMG4 set is
a constant in the same file.
