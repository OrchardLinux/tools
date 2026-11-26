# launch

```sh
orchard2 launch
```

starts a system installed on the device, from DFU mode, as Orchard (the
launcher) does. The device is taken to m1n1, which reads the boot entries on
the device's own boot partition and starts one.

- **One system installed**: it starts.
- **Several**: they are listed, the default under the mark. ↑ and ↓ (or j
  and k) move the mark and Enter starts that system; a number starts that
  row; Esc or q leaves without starting. Without a terminal the default
  starts.
- `--list` prints the systems (id, title, kernel, which is the default) and
  starts nothing. The device stays in m1n1; to start a system, put it in DFU
  mode again.
- `--entry ID` starts exactly that entry. An unknown id ends the run with
  exit status 2.
- **Nothing installed**: exit status 6.

The run ends when the start command is out; the system continues on the
device by itself.

Waiting for DFU mode, naming one device, the boards it works on and how the
device is entered are the same as for [install](install.md#waiting-for-dfu-mode).
That includes the guided run from recovery mode in a terminal, and the
usual DFU steps without one.
`--wait SECONDS` and `--log FILE` too.

Any Ctrl-C ends the run (exit status 8).

Events and exit statuses: [reference.md](reference.md#install-and-launch).
