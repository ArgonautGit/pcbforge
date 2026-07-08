# ORC-4 live gate — airflow interlock checklist

The unit tests (mocked modem lines) are green, but the done-when for ORC-4
requires live hardware: blocking the duct must flip a passing check to a
clear error naming the machine and dongle. Run this at the machines.

## Prerequisites

- The AIR-fiber / AIR-uv USB-serial dongles, each wired so the duct sail
  switch bridges RTS to CTS (switch closed = airflow present).
- Blower running on the machine under test.

## Steps (repeat per machine: `fiber`, then `uv`)

1. Plug the machine's AIR dongle into the orchestrator host. Find its port:

   ```sh
   cargo run -p orchestra --example airflow_check
   ```

   (with no argument it lists detected serial ports, e.g. `/dev/ttyUSB0`).

2. Export the mapping. Both variables must be set; only the port of the
   machine being checked is opened, so the other may be a placeholder:

   ```sh
   export PCBFORGE_AIR_FIBER=/dev/ttyUSB0
   export PCBFORGE_AIR_UV=/dev/ttyUSB1   # placeholder ok if not under test
   ```

3. With the blower on and the duct clear, run the live check:

   ```sh
   cargo run -p orchestra --example airflow_check -- fiber
   ```

   Expect exit code 0 and `OK: airflow confirmed on `fiber` — safe to lase`.

4. Block the duct (cover the intake or trip the sail by hand) and rerun the
   same command. Expect exit code 1 and an error of the form:

   > ERROR: no airflow on machine `fiber`: sail switch open (dongle
   > AIR-fiber at /dev/ttyUSB0 reports CTS low) — check the extraction duct
   > and blower before lasing

   Confirm the message names the correct machine and the correct dongle path.

5. Unblock the duct and rerun; the check must return to OK.

6. Repeat steps 1–5 for `uv`.

## Sign-off

- [ ] fiber: OK when clear, clear error when blocked, OK after unblocking
- [ ] uv: OK when clear, clear error when blocked, OK after unblocking

When both boxes are checked, ORC-4's live done-when is satisfied.
