# captures/

Real USB captures from the B4 (DRV-1), the input to the offline protocol decode
(DRV-2). Produced by the operator at the machine with `tools/capture.sh` per
`docs/capture-plan.md`. Empty until the first capture session.

- `MANIFEST.csv` — one row per capture (schema in docs/capture-plan.md §6);
  `sha256` is filled by `cargo xtask fixtures`.
- `<NN>-<slug>.pcapng` — one experiment per file, NN = 00..13.
- `00-descriptors.txt` — `lsusb -v` for the B4 (from experiment 00).
- `expected/<NN>.bin` — added later by DRV-2: the exact list payloads a
  reimplementation must reproduce.

Do not hand-edit the `.pcapng` files. Commit captures + RUNLOG.md together.
