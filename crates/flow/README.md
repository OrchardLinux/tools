# orchard-flow

The state of the graphical installer ("Install Omarchy A") and launcher
("Orchard"): one statechart and one function that changes it.

```text
update(state, event) -> (state, effects)
view(state)          -> device + screen
```

- **Events** come from the user (Continue, a tab, a size), from USB (a device
  attached or detached, in which mode) and from work in progress (iPadOS is up,
  a split finished, the install environment answered).
- **Effects** are names of work for the app to start: boot iPadOS with the
  overlay, start the split, list the boot entries. This crate never does any
  of that work itself.
- **The view** is derived from the state and nothing else. The app animates the
  difference between two views; the device drawing is persistent and morphs.

The crate has no dependencies and does no I/O, so every screen in the design is
a test in `src/tests.rs`.

## Non-goals

These are deliberate. A change that needs one of them is a change to this list
first.

- **palera1n only.** Two chains: DFU → palera1n → iPadOS with the overlay
  (split the storage), and DFU → palera1n → Pongo → m1n1 (install, launch). No
  other boot path and no abstraction over chains.
- **No command or transaction framework.** The split and the install are each
  one fixed sequence with their own resume rule.
- **One device at a time.** Several devices in DFU is a screen that asks to
  unplug the others, not a device picker.
- **One layout.** iPadOS plus one Omarchy A part. Everything outside iPadOS
  belongs to Omarchy A and is overwritten after an explicit warning.
- **No exploit or chain code here.** This crate names the work; the app's
  executors do it.
