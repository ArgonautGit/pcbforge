//! ORC-4 live airflow-interlock check.
//!
//! Usage:
//!   PCBFORGE_AIR_FIBER=/dev/ttyUSB0 PCBFORGE_AIR_UV=/dev/ttyUSB1 \
//!       cargo run -p orchestra --example airflow_check -- <fiber|uv>
//!
//! Exits 0 when airflow is detected, 1 with a clear error otherwise.
//! Run with no argument to list detected serial ports.

use std::process::ExitCode;

use orchestra::airflow::require_airflow;
use pcb_core::Machine;

fn list_ports() {
    match serialport::available_ports() {
        Ok(ports) if ports.is_empty() => println!("no serial ports detected"),
        Ok(ports) => {
            println!("detected serial ports:");
            for p in ports {
                println!("  {} ({:?})", p.port_name, p.port_type);
            }
        }
        Err(e) => println!("could not enumerate serial ports: {e}"),
    }
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let machine = match arg.as_deref() {
        Some("fiber") => Machine::Fiber,
        Some("uv") => Machine::Uv,
        Some(other) => {
            eprintln!("unknown machine `{other}`; expected `fiber` or `uv`");
            return ExitCode::FAILURE;
        }
        None => {
            eprintln!("usage: airflow_check <fiber|uv>");
            list_ports();
            return ExitCode::FAILURE;
        }
    };

    match require_airflow(machine) {
        Ok(()) => {
            println!("OK: airflow confirmed on `{}` — safe to lase", arg.unwrap());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            list_ports();
            ExitCode::FAILURE
        }
    }
}
