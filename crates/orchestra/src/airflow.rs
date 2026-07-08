//! ORC-4 — per-machine airflow interlock.
//!
//! Each laser machine has a sail switch in its extraction duct, wired to an
//! `AIR-<machine>` USB-serial dongle: the switch bridges RTS to CTS when the
//! sail is deflected by airflow. We assert RTS, wait a short settle delay,
//! then read CTS back. CTS high means air is moving; CTS low means the duct
//! is blocked (or the dongle/switch is disconnected) and the stage must not
//! emit laser.
//!
//! Modem-line method names verified against the pinned serialport 4.9.0
//! source (`~/.cargo/registry/src/index.crates.io-*/serialport-4.9.0/src/lib.rs`),
//! trait `serialport::SerialPort`:
//!   - `fn write_request_to_send(&mut self, level: bool) -> Result<()>` (line 563)
//!   - `fn read_clear_to_send(&mut self) -> Result<bool>` (line 591)

use std::env;
use std::fmt;
use std::thread;
use std::time::Duration;

use pcb_core::Machine;
use serialport::SerialPort;

/// Environment variable naming the fiber machine's AIR dongle port.
pub const ENV_AIR_FIBER: &str = "PCBFORGE_AIR_FIBER";
/// Environment variable naming the UV machine's AIR dongle port.
pub const ENV_AIR_UV: &str = "PCBFORGE_AIR_UV";

/// Time allowed for RTS to propagate through the sail switch to CTS.
pub const SETTLE_DELAY: Duration = Duration::from_millis(50);

/// Baud rate used when opening the dongle. Irrelevant to the modem lines,
/// but the port must be opened with some configuration.
const BAUD: u32 = 9600;

/// Stable lowercase name for a machine, used in errors and dongle labels.
pub fn machine_name(machine: Machine) -> &'static str {
    match machine {
        Machine::Fiber => "fiber",
        Machine::Uv => "uv",
    }
}

/// Errors from the airflow interlock. Every variant's `Display` names the
/// machine, and port-related variants name the dongle path as well.
#[derive(Debug)]
pub enum AirflowError {
    /// The sail switch did not close: CTS stayed low after asserting RTS.
    NoAirflow { machine: Machine, dongle: String },
    /// The dongle's serial port could not be opened or driven.
    Port {
        machine: Machine,
        dongle: String,
        source: serialport::Error,
    },
    /// The environment variable mapping the machine to its dongle is unset.
    ConfigMissing { machine: Machine, var: &'static str },
}

impl fmt::Display for AirflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AirflowError::NoAirflow { machine, dongle } => write!(
                f,
                "no airflow on machine `{name}`: sail switch open \
                 (dongle AIR-{name} at {dongle} reports CTS low) — \
                 check the extraction duct and blower before lasing",
                name = machine_name(*machine),
            ),
            AirflowError::Port {
                machine,
                dongle,
                source,
            } => write!(
                f,
                "airflow check failed on machine `{name}`: cannot drive \
                 dongle AIR-{name} at {dongle}: {source}",
                name = machine_name(*machine),
            ),
            AirflowError::ConfigMissing { machine, var } => write!(
                f,
                "airflow dongle for machine `{name}` is not configured: \
                 set {var} to the AIR-{name} serial port path",
                name = machine_name(*machine),
            ),
        }
    }
}

impl std::error::Error for AirflowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AirflowError::Port { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Maps each machine to the serial port path of its AIR dongle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirflowConfig {
    pub fiber_port: String,
    pub uv_port: String,
}

impl AirflowConfig {
    /// Loads the mapping from `PCBFORGE_AIR_FIBER` / `PCBFORGE_AIR_UV`.
    ///
    /// Only the port for the machine actually being checked is ever opened,
    /// so callers checking a single machine may set the other variable to a
    /// placeholder.
    pub fn from_env() -> Result<Self, AirflowError> {
        let read = |machine: Machine, var: &'static str| {
            env::var(var).map_err(|_| AirflowError::ConfigMissing { machine, var })
        };
        Ok(Self {
            fiber_port: read(Machine::Fiber, ENV_AIR_FIBER)?,
            uv_port: read(Machine::Uv, ENV_AIR_UV)?,
        })
    }

    /// The dongle port path for `machine`.
    pub fn port_for(&self, machine: Machine) -> &str {
        match machine {
            Machine::Fiber => &self.fiber_port,
            Machine::Uv => &self.uv_port,
        }
    }
}

/// The two modem control lines the sail-switch dongle uses. Abstracted so
/// the interlock logic can be unit-tested without hardware.
pub trait ModemLines {
    /// Asserts (raises) the RTS line.
    fn assert_rts(&mut self) -> Result<(), serialport::Error>;
    /// Reads whether the CTS line is asserted.
    fn read_cts(&mut self) -> Result<bool, serialport::Error>;
}

/// Production [`ModemLines`] backed by a real serial port.
pub struct SerialModemLines {
    port: Box<dyn SerialPort>,
}

impl SerialModemLines {
    pub fn new(port: Box<dyn SerialPort>) -> Self {
        Self { port }
    }

    /// Opens the dongle at `path` with a short timeout.
    pub fn open(path: &str) -> Result<Self, serialport::Error> {
        let port = serialport::new(path, BAUD)
            .timeout(Duration::from_millis(500))
            .open()?;
        Ok(Self::new(port))
    }
}

impl ModemLines for SerialModemLines {
    fn assert_rts(&mut self) -> Result<(), serialport::Error> {
        // serialport 4.9.0: SerialPort::write_request_to_send(true) asserts RTS.
        self.port.write_request_to_send(true)
    }

    fn read_cts(&mut self) -> Result<bool, serialport::Error> {
        // serialport 4.9.0: SerialPort::read_clear_to_send() returns CTS state.
        self.port.read_clear_to_send()
    }
}

/// Core interlock logic: assert RTS, let the line settle, read CTS.
///
/// CTS high (sail deflected by airflow) is `Ok(())`; CTS low is
/// [`AirflowError::NoAirflow`] naming the machine and dongle.
pub fn require_airflow_with(
    lines: &mut dyn ModemLines,
    machine: Machine,
    dongle: &str,
) -> Result<(), AirflowError> {
    let port_err = |source| AirflowError::Port {
        machine,
        dongle: dongle.to_owned(),
        source,
    };

    lines.assert_rts().map_err(port_err)?;
    thread::sleep(SETTLE_DELAY);
    match lines.read_cts().map_err(port_err)? {
        true => Ok(()),
        false => Err(AirflowError::NoAirflow {
            machine,
            dongle: dongle.to_owned(),
        }),
    }
}

/// Checks the sail switch for `machine` using the dongle mapped by `config`.
pub fn require_airflow_configured(
    config: &AirflowConfig,
    machine: Machine,
) -> Result<(), AirflowError> {
    let dongle = config.port_for(machine);
    let mut lines = SerialModemLines::open(dongle).map_err(|source| AirflowError::Port {
        machine,
        dongle: dongle.to_owned(),
        source,
    })?;
    require_airflow_with(&mut lines, machine, dongle)
}

/// Production entry point for laser-emitting stages: verifies extraction
/// airflow for `machine`, reading the dongle mapping from the environment.
pub fn require_airflow(machine: Machine) -> Result<(), AirflowError> {
    require_airflow_configured(&AirflowConfig::from_env()?, machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted modem lines: records RTS asserts, returns canned CTS values.
    struct MockLines {
        rts_result: Result<(), serialport::Error>,
        cts_result: Result<bool, serialport::Error>,
        rts_asserted: bool,
        cts_read_after_rts: bool,
    }

    impl MockLines {
        fn new(
            rts_result: Result<(), serialport::Error>,
            cts_result: Result<bool, serialport::Error>,
        ) -> Self {
            Self {
                rts_result,
                cts_result,
                rts_asserted: false,
                cts_read_after_rts: false,
            }
        }
    }

    impl ModemLines for MockLines {
        fn assert_rts(&mut self) -> Result<(), serialport::Error> {
            self.rts_asserted = true;
            self.rts_result.clone()
        }

        fn read_cts(&mut self) -> Result<bool, serialport::Error> {
            self.cts_read_after_rts = self.rts_asserted;
            self.cts_result.clone()
        }
    }

    fn io_err(msg: &str) -> serialport::Error {
        serialport::Error::new(serialport::ErrorKind::Io(std::io::ErrorKind::Other), msg)
    }

    #[test]
    fn cts_high_means_airflow_ok() {
        let mut lines = MockLines::new(Ok(()), Ok(true));
        let r = require_airflow_with(&mut lines, Machine::Fiber, "/dev/ttyUSB0");
        assert!(r.is_ok());
        assert!(lines.rts_asserted, "RTS must be asserted");
        assert!(lines.cts_read_after_rts, "CTS must be read after RTS");
    }

    #[test]
    fn cts_low_names_machine_and_dongle() {
        let mut lines = MockLines::new(Ok(()), Ok(false));
        let err = require_airflow_with(&mut lines, Machine::Uv, "/dev/ttyACM3").unwrap_err();
        assert!(matches!(err, AirflowError::NoAirflow { .. }));
        let msg = err.to_string();
        assert!(msg.contains("uv"), "missing machine name: {msg}");
        assert!(msg.contains("/dev/ttyACM3"), "missing dongle: {msg}");
        assert!(msg.contains("AIR-uv"), "missing dongle label: {msg}");
    }

    #[test]
    fn rts_failure_propagates_with_context() {
        let mut lines = MockLines::new(Err(io_err("rts pin dead")), Ok(true));
        let err = require_airflow_with(&mut lines, Machine::Fiber, "/dev/ttyUSB7").unwrap_err();
        assert!(matches!(err, AirflowError::Port { .. }));
        let msg = err.to_string();
        assert!(msg.contains("fiber"), "missing machine name: {msg}");
        assert!(msg.contains("/dev/ttyUSB7"), "missing dongle: {msg}");
        assert!(msg.contains("rts pin dead"), "missing source: {msg}");
    }

    #[test]
    fn cts_read_failure_propagates() {
        let mut lines = MockLines::new(Ok(()), Err(io_err("modem status ioctl failed")));
        let err = require_airflow_with(&mut lines, Machine::Uv, "/dev/ttyUSB1").unwrap_err();
        assert!(matches!(err, AirflowError::Port { .. }));
        assert!(err.to_string().contains("/dev/ttyUSB1"));
    }

    #[test]
    fn config_maps_machines_to_ports() {
        let cfg = AirflowConfig {
            fiber_port: "/dev/ttyUSB0".into(),
            uv_port: "/dev/ttyUSB1".into(),
        };
        assert_eq!(cfg.port_for(Machine::Fiber), "/dev/ttyUSB0");
        assert_eq!(cfg.port_for(Machine::Uv), "/dev/ttyUSB1");
    }

    #[test]
    fn missing_config_error_names_machine_and_var() {
        let err = AirflowError::ConfigMissing {
            machine: Machine::Fiber,
            var: ENV_AIR_FIBER,
        };
        let msg = err.to_string();
        assert!(msg.contains("fiber"), "{msg}");
        assert!(msg.contains("PCBFORGE_AIR_FIBER"), "{msg}");
    }
}
