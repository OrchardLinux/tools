//! The host's serial port for a CDC serial function of a device.
//!
//! macOS and Linux bind their own driver to a CDC serial function and offer
//! it as a serial port; macOS does not let another program claim the
//! interface. So the stream goes through that port, found by the device's
//! USB location and the function's interface numbers.
use crate::error::{Fault, Kind};
use crate::wire::Serial;
use crate::Device;
use std::path::PathBuf;
use std::time::Duration;

/// A CDC serial function: its control and its data interface. macOS hangs
/// the port below the data interface, Linux below the control interface.
pub(crate) type Function = [u8; 2];

/// Opens the serial port of `function` on the device at `device.location`.
#[cfg(unix)]
pub(crate) fn open(device: &Device, function: Function) -> Result<Tty, Fault> {
    let path = find(&device.location, function)?.ok_or_else(|| {
        Fault::new(
            Kind::Disconnected,
            format!("no serial port for the device at {}", device.location),
        )
    })?;
    Tty::open(&path)
}

#[cfg(not(unix))]
pub(crate) fn open(_device: &Device, _function: Function) -> Result<Tty, Fault> {
    Err(Fault::new(
        Kind::Other,
        "serial ports are only supported on macOS and Linux",
    ))
}

#[cfg(not(unix))]
pub(crate) struct Tty;

#[cfg(not(unix))]
impl Serial for Tty {
    fn read(&mut self, _data: &mut [u8], _timeout: Duration) -> Result<usize, Fault> {
        unreachable!("no serial port can be opened on this host")
    }
    fn write(&mut self, _data: &[u8], _timeout: Duration) -> Result<(), Fault> {
        unreachable!("no serial port can be opened on this host")
    }
}

#[cfg(target_os = "macos")]
fn find(location: &str, function: Function) -> Result<Option<PathBuf>, Fault> {
    let location = location
        .strip_prefix("0x")
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .ok_or_else(|| Fault::invalid(format!("{location} is not a USB location")))?;
    registry::port(&registry::read()?, location, function)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn find(location: &str, function: Function) -> Result<Option<PathBuf>, Fault> {
    let io = |e: std::io::Error| Fault::new(Kind::Other, format!("/sys/class/tty: {e}"));
    for entry in std::fs::read_dir("/sys/class/tty").map_err(io)? {
        let name = entry.map_err(io)?.file_name();
        // The tty's `device` link ends in the USB interface it belongs to.
        let Ok(interface) = std::fs::canonicalize(
            std::path::Path::new("/sys/class/tty")
                .join(&name)
                .join("device"),
        ) else {
            continue;
        };
        let interface = interface.file_name().and_then(|n| n.to_str());
        if interface.is_some_and(|i| sysfs_interface_matches(i, location, function)) {
            return Ok(Some(std::path::Path::new("/dev").join(name)));
        }
    }
    Ok(None)
}

/// Whether a sysfs interface name such as `1-2.3:1.0` (bus, port chain,
/// configuration, interface) is `function` of the device at `location`,
/// which discovery writes as `001-2.3`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn sysfs_interface_matches(name: &str, location: &str, function: Function) -> bool {
    let parsed = || {
        let (device, interface) = name.split_once(':')?;
        let (_configuration, interface) = interface.split_once('.')?;
        let (bus, chain) = device.split_once('-')?;
        let (wanted_bus, wanted_chain) = location.split_once('-')?;
        Some(
            bus.parse::<u32>().ok()? == wanted_bus.parse::<u32>().ok()?
                && chain == wanted_chain
                && function.contains(&interface.parse::<u8>().ok()?),
        )
    };
    parsed().unwrap_or(false)
}

/// Finding the port in the macOS I/O registry, as `ioreg` prints it.
#[cfg(target_os = "macos")]
mod registry {
    use super::*;
    use plist::Value;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    /// `ioreg` answers in well under a second; a hung one must not hang us.
    const LIMIT: Duration = Duration::from_secs(10);

    /// All USB devices with everything below them, as a property list.
    pub fn read() -> Result<Value, Fault> {
        let failed = |why: String| Fault::new(Kind::Other, format!("ioreg: {why}"));
        let mut child = Command::new("/usr/sbin/ioreg")
            .args(["-a", "-l", "-r", "-c", "IOUSBHostDevice"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| failed(e.to_string()))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::thread::spawn(move || {
            let mut text = Vec::new();
            stdout.read_to_end(&mut text).map(|_| text)
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait().map_err(|e| failed(e.to_string()))? {
                Some(status) => break status,
                None if started.elapsed() > LIMIT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Fault::new(Kind::Timeout, "ioreg did not finish"));
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        let text = reader
            .join()
            .expect("reader does not panic")
            .map_err(|e| failed(e.to_string()))?;
        if !status.success() {
            return Err(failed(status.to_string()));
        }
        // With no USB device at all, ioreg prints nothing.
        if text.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        Value::from_reader_xml(std::io::Cursor::new(text)).map_err(|e| failed(e.to_string()))
    }

    fn children(node: &Value) -> &[Value] {
        node.as_dictionary()
            .and_then(|d| d.get("IORegistryEntryChildren"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn integer(node: &Value, key: &str) -> Option<u64> {
        node.as_dictionary()?.get(key)?.as_unsigned_integer()
    }

    fn string<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
        node.as_dictionary()?.get(key)?.as_string()
    }

    /// Collects the serial ports below `node`, each with the number of the
    /// USB interface it hangs under.
    fn ports(node: &Value, inherited: Option<u64>, found: &mut Vec<(u64, PathBuf)>) {
        let interface = integer(node, "bInterfaceNumber").or(inherited);
        if string(node, "IOObjectClass") == Some("IOSerialBSDClient") {
            if let (Some(interface), Some(path)) = (interface, string(node, "IOCalloutDevice")) {
                found.push((interface, path.into()));
            }
        }
        for child in children(node) {
            ports(child, interface, found);
        }
    }

    fn devices(node: &Value, location: u64, found: &mut Vec<(u64, PathBuf)>) {
        if string(node, "IOObjectClass") == Some("IOUSBHostDevice")
            && integer(node, "locationID") == Some(location)
        {
            ports(node, None, found);
            return;
        }
        for child in children(node) {
            devices(child, location, found);
        }
    }

    /// The port of `function` on the device at `location`.
    pub fn port(
        registry: &Value,
        location: u64,
        function: Function,
    ) -> Result<Option<PathBuf>, Fault> {
        let mut found = Vec::new();
        for root in registry.as_array().map(Vec::as_slice).unwrap_or(&[]) {
            devices(root, location, &mut found);
        }
        found.retain(|(interface, _)| function.iter().any(|f| u64::from(*f) == *interface));
        found.sort();
        found.dedup();
        match found.len() {
            0 => Ok(None),
            1 => Ok(Some(found.remove(0).1)),
            n => Err(Fault::unexpected(format!(
                "{n} serial ports for one USB function"
            ))),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use plist::Dictionary;

        fn serial(interface: u64, path: &str) -> Value {
            let mut client = Dictionary::new();
            client.insert("IOObjectClass".into(), "IOSerialBSDClient".into());
            client.insert("IOCalloutDevice".into(), path.into());
            let mut node = Dictionary::new();
            node.insert("bInterfaceNumber".into(), interface.into());
            node.insert(
                "IORegistryEntryChildren".into(),
                Value::Array(vec![Value::Dictionary(client)]),
            );
            Value::Dictionary(node)
        }

        fn device(location: u64, interfaces: Vec<Value>) -> Value {
            let mut node = Dictionary::new();
            node.insert("IOObjectClass".into(), "IOUSBHostDevice".into());
            node.insert("locationID".into(), location.into());
            node.insert("IORegistryEntryChildren".into(), Value::Array(interfaces));
            Value::Dictionary(node)
        }

        #[test]
        fn picks_the_function_on_the_device_at_the_location() {
            let registry = Value::Array(vec![
                device(
                    0x100000,
                    vec![serial(3, "/dev/cu.console"), serial(1, "/dev/cu.proxy")],
                ),
                device(0x200000, vec![serial(1, "/dev/cu.other")]),
            ]);
            let port = |location, function| port(&registry, location, function).unwrap();
            assert_eq!(port(0x100000, [0, 1]), Some("/dev/cu.proxy".into()));
            assert_eq!(port(0x100000, [2, 3]), Some("/dev/cu.console".into()));
            assert_eq!(port(0x200000, [0, 1]), Some("/dev/cu.other".into()));
            assert_eq!(port(0x200000, [2, 3]), None);
            assert_eq!(port(0x300000, [0, 1]), None);
        }

        #[test]
        fn two_ports_for_one_function_is_an_error() {
            let registry = Value::Array(vec![device(
                0x100000,
                vec![serial(1, "/dev/cu.a"), serial(1, "/dev/cu.b")],
            )]);
            assert_eq!(
                port(&registry, 0x100000, [0, 1]).unwrap_err().kind,
                Kind::Unexpected
            );
        }
    }
}

/// An open serial port in raw mode.
#[cfg(unix)]
pub(crate) struct Tty(std::fs::File);

#[cfg(unix)]
fn os(what: &str) -> Fault {
    let e = std::io::Error::last_os_error();
    let kind = match e.kind() {
        std::io::ErrorKind::PermissionDenied => Kind::Denied,
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ResourceBusy => Kind::Busy,
        std::io::ErrorKind::NotFound => Kind::Disconnected,
        _ => match e.raw_os_error() {
            // What a serial port answers once its device is unplugged.
            Some(libc::ENXIO) | Some(libc::EIO) | Some(libc::ENODEV) => Kind::Disconnected,
            _ => Kind::Other,
        },
    };
    Fault::new(kind, format!("{what}: {e}"))
}

#[cfg(unix)]
impl Tty {
    fn open(path: &std::path::Path) -> Result<Tty, Fault> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let name = path.display().to_string();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| os(&name))?;
        let fd = file.as_raw_fd();
        // SAFETY: `fd` is open for the whole block, and `term` is filled by
        // tcgetattr before it is read.
        unsafe {
            // One program per port: two would steal each other's replies.
            if libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) != 0 {
                return Err(os(&name));
            }
            let mut term = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut term) != 0 {
                return Err(os(&name));
            }
            libc::cfmakeraw(&mut term);
            term.c_cflag |= libc::CLOCAL | libc::CREAD;
            // Drop what an earlier user of the port left unread.
            if libc::tcsetattr(fd, libc::TCSANOW, &term) != 0
                || libc::tcflush(fd, libc::TCIOFLUSH) != 0
            {
                return Err(os(&name));
            }
        }
        Ok(Tty(file))
    }

    /// Waits until the port can be read or written.
    fn ready(&self, events: libc::c_short, timeout: Duration) -> Result<(), Fault> {
        use std::os::fd::AsRawFd;
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events,
            revents: 0,
        };
        let ms = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: `poll` is one valid pollfd, used only during the call.
        match unsafe { libc::poll(&mut poll, 1, ms) } {
            0 => Err(Fault::new(Kind::Timeout, "timed out")),
            n if n < 0 => Err(os("serial port")),
            _ => Ok(()),
        }
    }
}

#[cfg(unix)]
impl Serial for Tty {
    fn read(&mut self, data: &mut [u8], timeout: Duration) -> Result<usize, Fault> {
        use std::io::Read;
        self.ready(libc::POLLIN, timeout)?;
        match self.0.read(data) {
            Ok(0) => Err(Fault::new(Kind::Disconnected, "the serial port closed")),
            Ok(n) => Ok(n),
            Err(_) => Err(os("serial port")),
        }
    }

    fn write(&mut self, mut data: &[u8], timeout: Duration) -> Result<(), Fault> {
        use std::io::Write;
        let deadline = crate::wire::Deadline::after(timeout);
        while !data.is_empty() {
            self.ready(libc::POLLOUT, deadline.left()?)?;
            match self.0.write(data) {
                Ok(0) => return Err(Fault::new(Kind::Disconnected, "the serial port closed")),
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(os("serial port")),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysfs_names() {
        let matches = |name, location| sysfs_interface_matches(name, location, [0, 1]);
        assert!(matches("1-2.3:1.0", "001-2.3"));
        assert!(matches("1-2.3:1.1", "001-2.3"));
        // The console function, another port, another bus.
        assert!(!matches("1-2.3:1.2", "001-2.3"));
        assert!(!matches("1-2.4:1.0", "001-2.3"));
        assert!(!matches("2-2.3:1.0", "001-2.3"));
        assert!(!matches("ttyS0", "001-2.3"));
    }
}
