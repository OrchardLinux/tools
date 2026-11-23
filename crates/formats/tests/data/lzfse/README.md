Synthetic LZFSE streams written by Apple's libcompression encoder on macOS.
No firmware: the inputs are text, byte ramps and a repeating pattern.

- `lzvn.lzfse`: one `bvxn` (LZVN) block
- `v2.lzfse`: one `bvx2` (FSE, packed header) block
- `v1.lzfse`: the same data as `v2`, with a `bvx1` (FSE, plain header) block
- `multi.lzfse`: several `bvx2` blocks with matches across block boundaries

`tests/lzfse.rs` holds the decoded size and SHA-256 of each.
