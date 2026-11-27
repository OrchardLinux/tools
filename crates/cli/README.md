# orchard

The command line. Its documentation is in [`docs/`](../../docs/README.md):

- [install](../../docs/install.md) and [launch](../../docs/launch.md), a
  second front end of the installer's and the launcher's flow
  (`crates/flow`, run by `crates/runtime`);
- [build](../../docs/build.md) and [cache](../../docs/cache.md)
  (`crates/build`, whose README describes how it works inside);
- [shell, exec, push and pull](../../docs/remote.md) (`crates/remote`);
- [run](../../docs/run.md), a RAM boot by a boot route
  (`crates/runtime`);
- [devices](../../docs/devices.md) and the
  [low-level commands](../../docs/low-level.md) (`crates/device`,
  `crates/usb`);
- [board data and firmware](../../docs/data-and-firmware.md)
  (`crates/boards`);
- [options, output, exit statuses and the environment](../../docs/reference.md).

This crate reads the arguments (`src/args.rs`), calls the libraries and
prints the result (`src/output.rs`); `install` and `launch` draw the flow on
the terminal (`src/commands/install.rs`, `src/commands/term.rs`).
