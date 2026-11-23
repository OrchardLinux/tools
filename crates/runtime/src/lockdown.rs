//! What a device running iOS or iPadOS says about itself before it trusts
//! this computer: its lockdown service (port 62078, through usbmux) answers
//! a few values without pairing. The installer asks for the model and the
//! ECID, to recognise a device that was plugged in running iPadOS instead
//! of in DFU mode (screen 1.2).
//!
//! lockdown's framing: a 4-byte big-endian length, then an XML property list.
use crate::usbmux;
use orchard_cache::{Code, Error, Result};
use plist::{Dictionary, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const PORT: u16 = 62078;
const MAX_MESSAGE: u32 = 1 << 20;

/// What the device answered; a value it keeps to itself is None.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answers {
    /// Lowercase hex without padding.
    pub ecid: Option<String>,
    /// e.g. iPad7,5.
    pub product: Option<String>,
}

fn request(stream: &mut UnixStream, body: Dictionary) -> Result<Dictionary> {
    let mut xml = Vec::new();
    Value::Dictionary(body).to_writer_xml(&mut xml).map_err(|e| Error::new(Code::Network, e.to_string()))?;
    stream.write_all(&(xml.len() as u32).to_be_bytes())?;
    stream.write_all(&xml)?;
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len);
    if len == 0 || len > MAX_MESSAGE {
        return Err(Error::new(Code::Network, "lockdown sent an unexpected message"));
    }
    let mut reply = vec![0u8; len as usize];
    stream.read_exact(&mut reply)?;
    Value::from_reader(std::io::Cursor::new(reply))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| Error::new(Code::Network, "lockdown's answer is not a property list"))
}

fn get(stream: &mut UnixStream, key: &str) -> Result<Option<Value>> {
    let mut body = Dictionary::new();
    body.insert("Label".into(), "orchard-installer".into());
    body.insert("Request".into(), "GetValue".into());
    body.insert("Key".into(), key.into());
    let reply = request(stream, body)?;
    // An unpaired computer gets an Error (e.g. GetProhibited) for most keys.
    Ok(if reply.contains_key("Error") { None } else { reply.get("Value").cloned() })
}

/// Asks the device with this UDID, through usbmuxd at `socket`.
pub fn ask(socket: &Path, udid: &str) -> Result<Answers> {
    let mut stream = usbmux::connect(socket, udid, PORT)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut query = Dictionary::new();
    query.insert("Label".into(), "orchard-installer".into());
    query.insert("Request".into(), "QueryType".into());
    let kind = request(&mut stream, query)?;
    if kind.get("Type").and_then(Value::as_string) != Some("com.apple.mobile.lockdown") {
        return Err(Error::new(Code::Network, "port 62078 is not lockdown"));
    }
    let product = get(&mut stream, "ProductType")?.and_then(|v| v.as_string().map(str::to_owned));
    let ecid = get(&mut stream, "UniqueChipID")?.and_then(|v| v.as_unsigned_integer()).map(|n| format!("{n:x}"));
    Ok(Answers { ecid, product })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// lockdown on one end of a socket pair: answers the model, keeps the ECID.
    fn fake(mut device: UnixStream) {
        std::thread::spawn(move || {
            let read = |s: &mut UnixStream| -> Option<Dictionary> {
                let mut len = [0u8; 4];
                s.read_exact(&mut len).ok()?;
                let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
                s.read_exact(&mut body).ok()?;
                Value::from_reader(std::io::Cursor::new(body)).ok()?.into_dictionary()
            };
            let write = |s: &mut UnixStream, d: Dictionary| {
                let mut xml = Vec::new();
                Value::Dictionary(d).to_writer_xml(&mut xml).unwrap();
                s.write_all(&(xml.len() as u32).to_be_bytes()).unwrap();
                s.write_all(&xml).unwrap();
            };
            while let Some(m) = read(&mut device) {
                let mut reply = Dictionary::new();
                match (m.get("Request").and_then(Value::as_string), m.get("Key").and_then(Value::as_string)) {
                    (Some("QueryType"), _) => drop(reply.insert("Type".into(), "com.apple.mobile.lockdown".into())),
                    (Some("GetValue"), Some("ProductType")) => drop(reply.insert("Value".into(), "iPad7,5".into())),
                    _ => drop(reply.insert("Error".into(), "GetProhibited".into())),
                }
                write(&mut device, reply);
            }
        });
    }

    #[test]
    fn values_the_device_keeps_come_back_as_none() {
        let (mut host, device) = UnixStream::pair().unwrap();
        fake(device);
        let mut query = Dictionary::new();
        query.insert("Request".into(), "QueryType".into());
        assert_eq!(request(&mut host, query).unwrap().get("Type").and_then(Value::as_string), Some("com.apple.mobile.lockdown"));
        assert_eq!(get(&mut host, "ProductType").unwrap().and_then(|v| v.as_string().map(str::to_owned)).as_deref(), Some("iPad7,5"));
        assert!(get(&mut host, "UniqueChipID").unwrap().is_none());
    }
}
