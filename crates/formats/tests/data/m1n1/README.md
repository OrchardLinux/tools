Synthetic, non-bootable inputs for packing an m1n1 image: a stand-in for
m1n1, two device trees, a gzip "kernel" and an "initramfs".

`tests/m1n1.rs` packs them and compares the SHA-256 of the result with what
orchard's Python assembler (`src/orchard/image/assemble.py`) produced from
the same inputs, with the boot arguments `console=tty0 rdinit=/init`:

- one device tree, no logo
- device trees in the order B, A, with a logo whose pixels are the bytes
  0 to 255 repeated
