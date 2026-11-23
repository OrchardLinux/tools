//! A connection to a TCP port on a device running iOS or iPadOS, through
//! the computer's usbmuxd (the daemon Finder and iTunes use; on Linux the
//! `usbmuxd` package). The companion's helper listens on such a port.
//!
//! usbmuxd's protocol: a 16-byte little-endian header (length including the
//! header, version 1, message type 8 for property lists, tag), then an XML
//! property list. After a successful `Connect`, the socket carries the
//! device port's bytes.
use orchard_cache::{Code, Error, Result};
use plist::{Dictionary, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub const SOCKET: &str = "/var/run/usbmuxd";
const PLIST: u32 = 8;
const VERSION: u32 = 1;
const MAX_MESSAGE: u32 = 1 << 20;

/// A device usbmuxd knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// usbmuxd's number for it, valid while it stays connected.
    pub id: u64,
    pub udid: String,
    pub usb: bool,
}

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::Network, message.into())
}

fn send(stream: &mut UnixStream, tag: u32, message: Dictionary) -> Result<()> {
    let mut body = Vec::new();
    Value::Dictionary(message).to_writer_xml(&mut body).map_err(|e| fail(e.to_string()))?;
    let mut out = Vec::with_capacity(16 + body.len());
    for v in [16 + body.len() as u32, VERSION, PLIST, tag] {
        out.extend(v.to_le_bytes());
    }
    out.extend(body);
    stream.write_all(&out)?;
    Ok(())
}

fn receive(stream: &mut UnixStream) -> Result<Dictionary> {
    let mut header = [0u8; 16];
    stream.read_exact(&mut header)?;
    let field = |i: usize| u32::from_le_bytes(header[4 * i..4 * i + 4].try_into().unwrap());
    let len = field(0);
    if !(16..=MAX_MESSAGE).contains(&len) || field(2) != PLIST {
        return Err(fail("usbmuxd sent an unexpected message"));
    }
    let mut body = vec![0u8; len as usize - 16];
    stream.read_exact(&mut body)?;
    Value::from_reader(std::io::Cursor::new(body))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| fail("usbmuxd's message is not a property list"))
}

fn request(kind: &str) -> Dictionary {
    let mut d = Dictionary::new();
    d.insert("MessageType".into(), kind.into());
    d.insert("ClientVersionString".into(), "orchard-installer".into());
    d.insert("ProgName".into(), "orchard-installer".into());
    d
}

fn open(socket: &Path) -> Result<UnixStream> {
    let stream = UnixStream::connect(socket).map_err(|e| {
        Error::new(Code::NotFound, format!("usbmuxd is not running ({e})"))
            .hint("on Linux, install and start the usbmuxd package")
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    Ok(stream)
}

/// The devices usbmuxd knows.
pub fn devices(socket: &Path) -> Result<Vec<Device>> {
    let mut stream = open(socket)?;
    send(&mut stream, 1, request("ListDevices"))?;
    let reply = receive(&mut stream)?;
    let list = reply.get("DeviceList").and_then(Value::as_array).ok_or_else(|| fail("usbmuxd listed no devices"))?;
    let mut out = Vec::new();
    for entry in list {
        let Some(d) = entry.as_dictionary() else { continue };
        let props = d.get("Properties").and_then(Value::as_dictionary);
        let id = d.get("DeviceID").or_else(|| props?.get("DeviceID")).and_then(Value::as_unsigned_integer);
        let udid = props.and_then(|p| p.get("SerialNumber")).and_then(Value::as_string);
        let usb = props.and_then(|p| p.get("ConnectionType")).and_then(Value::as_string) == Some("USB");
        if let (Some(id), Some(udid)) = (id, udid) {
            out.push(Device { id, udid: udid.to_ascii_lowercase(), usb });
        }
    }
    Ok(out)
}

/// A stream to `port` on the device with this UDID, over USB.
pub fn connect(socket: &Path, udid: &str, port: u16) -> Result<UnixStream> {
    let udid = udid.to_ascii_lowercase();
    let device = devices(socket)?
        .into_iter()
        .find(|d| d.usb && d.udid.replace('-', "") == udid.replace('-', ""))
        .ok_or_else(|| Error::new(Code::NotFound, "usbmuxd doesn't see the device on USB"))?;
    let mut stream = open(socket)?;
    let mut message = request("Connect");
    message.insert("DeviceID".into(), Value::Integer(device.id.into()));
    // usbmuxd takes the port in network byte order.
    message.insert("PortNumber".into(), Value::Integer(u64::from(port.swap_bytes()).into()));
    send(&mut stream, 2, message)?;
    let reply = receive(&mut stream)?;
    match reply.get("Number").and_then(Value::as_unsigned_integer) {
        Some(0) => {
            stream.set_read_timeout(None)?;
            Ok(stream)
        }
        // 3: connection refused by the device (nothing listens on the port).
        Some(3) => Err(Error::new(Code::NotFound, format!("nothing listens on port {port} on the device"))),
        other => Err(fail(format!("usbmuxd refused the connection ({other:?})"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A usbmuxd that knows one device and echoes on its port 4242.
    fn fake(path: &Path) {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut s = stream.unwrap();
                let Ok(m) = receive(&mut s) else { continue };
                let mut reply = Dictionary::new();
                match m.get("MessageType").and_then(Value::as_string) {
                    Some("ListDevices") => {
                        let mut props = Dictionary::new();
                        props.insert("SerialNumber".into(), "0123456789ABCDEF0123456789ABCDEF01234567".into());
                        props.insert("ConnectionType".into(), "USB".into());
                        let mut d = Dictionary::new();
                        d.insert("DeviceID".into(), Value::Integer(7.into()));
                        d.insert("Properties".into(), Value::Dictionary(props));
                        reply.insert("DeviceList".into(), Value::Array(vec![Value::Dictionary(d)]));
                        send(&mut s, 1, reply).unwrap();
                    }
                    Some("Connect") => {
                        let port = m.get("PortNumber").and_then(Value::as_unsigned_integer).unwrap() as u16;
                        let ok = m.get("DeviceID").and_then(Value::as_unsigned_integer) == Some(7) && port.swap_bytes() == 4242;
                        reply.insert("MessageType".into(), "Result".into());
                        reply.insert("Number".into(), Value::Integer(if ok { 0 } else { 3 }.into()));
                        send(&mut s, 2, reply).unwrap();
                        if ok {
                            let mut buf = [0u8; 5];
                            s.read_exact(&mut buf).unwrap();
                            s.write_all(&buf).unwrap();
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    #[test]
    fn lists_devices_and_connects_to_a_port() {
        let path = std::env::temp_dir().join(format!("usbmuxd-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        fake(&path);
        let listed = devices(&path).unwrap();
        assert_eq!(listed, [Device { id: 7, udid: "0123456789abcdef0123456789abcdef01234567".into(), usb: true }]);
        let mut s = connect(&path, "0123456789ABCDEF0123456789ABCDEF01234567", 4242).unwrap();
        s.write_all(b"hello").unwrap();
        let mut back = [0u8; 5];
        s.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"hello");
        let refused = connect(&path, "0123456789abcdef0123456789abcdef01234567", 1).unwrap_err();
        assert!(refused.to_string().contains("nothing listens"));
        assert!(connect(&path, "ffff", 4242).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
