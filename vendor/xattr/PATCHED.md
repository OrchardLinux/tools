# Orchard's copy of xattr 0.2.3

Unchanged from crates.io except `src/sys/mod.rs`: on Linux, `ENOATTR` is
`ENODATA` (libc no longer defines `ENOATTR` there), so the crate builds for
Linux with current libc. zed-async-tar (through gpui) depends on it.
