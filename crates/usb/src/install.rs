//! The install protocol, host side: drives the install system's raw USB
//! receiver (`orchard-install-usb`) as `docs/install-protocol.md` (protocol
//! 1) describes. No network, no SSH, no keys.
//!
//! One request at a time; the device's answer is `Ok` or `Error`, and `Event`s
//! may come at any time. A thread reads the IN endpoint all the time, so
//! events never back up while the host is writing a long transfer. Every
//! message, heartbeats included, resets the silence timer: a host that hears
//! nothing for [`SILENCE`] treats the link as lost, and the device keeps going.
use crate::error::{Fault, Kind};
use crate::{Device, Error};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

pub const VENDOR: u16 = 0x05ac;
pub const PRODUCT: u16 = 0x4146;
pub const CLASS: u8 = 0xff;
pub const SUBCLASS: u8 = 0x4f;
pub const PROTOCOL: u8 = 0x01;
/// The protocol version this host speaks.
pub const VERSION: u64 = 1;
/// The largest payload of one message.
pub const MAX_PAYLOAD: usize = 1 << 20;
/// How long the device may stay silent before the link counts as lost; it
/// sends a heartbeat every second.
pub const SILENCE: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Type {
    Info = 0x01,
    Inspect = 0x02,
    Put = 0x03,
    Plan = 0x04,
    Stream = 0x05,
    Data = 0x06,
    Ok = 0x81,
    Error = 0x82,
    Event = 0x83,
}

impl Type {
    fn from(byte: u8) -> Option<Type> {
        Some(match byte {
            0x81 => Type::Ok,
            0x82 => Type::Error,
            0x83 => Type::Event,
            _ => return None,
        })
    }
}

/// ORCHARD_USB_COMPAT=1: talk to receivers from before they read whole
/// transfers (images up to dev-20261121-205416): header and payload apart,
/// 64 KiB Data messages.
fn compat() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ORCHARD_USB_COMPAT").is_some())
}

/// One message as it is on the wire.
fn frame(kind: Type, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&[kind as u8, 0, 0, 0]);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// What the device said no to, by its code. Hosts act on codes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Failure {
    /// The device answered with an error.
    Refused(Refusal),
    /// The device speaks another protocol version.
    Version(u64),
    /// The USB link failed or went silent ([`Kind::Timeout`]: lost).
    Usb(Error),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Refused(r) => write!(f, "{}: {}", r.code, r.message),
            Failure::Version(v) => write!(f, "the install system speaks protocol {v}, this installer {VERSION}"),
            Failure::Usb(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Failure {}

impl Failure {
    /// The link was lost: unplugged, or silent for [`SILENCE`].
    pub fn lost(&self) -> bool {
        matches!(self, Failure::Usb(e) if matches!(e.kind, Kind::Timeout | Kind::Disconnected))
    }
}

fn usb(action: &'static str, fault: Fault) -> Failure {
    Failure::Usb(Error { action, kind: fault.kind, detail: fault.detail })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Ready,
    Planned,
    StreamIncomplete,
    Finishing,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Streaming {
    pub written: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Info {
    pub protocol: u64,
    /// Lowercase hex without padding; None when the device couldn't read it.
    pub ecid: Option<String>,
    /// The short board name, as in the serial number (`j71b`).
    pub board: String,
    pub boot_id: String,
    pub phase: Phase,
    #[serde(default)]
    pub staged: Vec<String>,
    pub stream: Option<Streaming>,
    /// The completion record, once `done`.
    pub record: Option<Value>,
    pub error: Option<Refusal>,
}

/// Something the device reports by itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Heartbeat { phase: Phase },
    Stream { written: u64, bytes: u64 },
    /// A finishing step started: `filesystem`, `layer`, `kernel`, `firmware`,
    /// `account`, `entry` or `sync`.
    Step(String),
    Done(Value),
    Failed(Refusal),
    /// An event this host doesn't know; newer receivers may send more.
    Other(Value),
}

impl Event {
    fn parse(value: Value) -> Event {
        let get = |k: &str| value.get(k).cloned();
        let text = |k: &str| value.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let num = |k: &str| value.get(k).and_then(Value::as_u64);
        match value.get("event").and_then(Value::as_str) {
            Some("heartbeat") => match get("phase").and_then(|p| serde_json::from_value(p).ok()) {
                Some(phase) => Event::Heartbeat { phase },
                None => Event::Other(value),
            },
            Some("stream") => match (num("written"), num("bytes")) {
                (Some(written), Some(bytes)) => Event::Stream { written, bytes },
                _ => Event::Other(value),
            },
            Some("step") => Event::Step(text("step")),
            Some("done") => Event::Done(get("record").unwrap_or(Value::Null)),
            Some("failed") => Event::Failed(Refusal { code: text("code"), message: text("message") }),
            _ => Event::Other(value),
        }
    }
}

/// The OUT direction: whole messages, written in order.
pub(crate) trait Outbound: Send {
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<(), Fault>;
}

/// The IN direction: whatever the next transfer brings. A timeout is not an
/// error here; silence is judged by the session.
pub(crate) trait Inbound: Send + 'static {
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, Fault>;
}

enum Incoming {
    Answer(Type, Vec<u8>),
    /// An event came (the handler has it): the device is alive, which keeps
    /// a request waiting for a slow answer from counting as silence.
    Alive,
}

/// Splits the byte stream into messages, forever, until the link fails.
/// The session's event handler, shared with its reader thread.
type OnEvent = std::sync::Arc<std::sync::Mutex<Box<dyn FnMut(&Event) + Send>>>;

/// Reads continuously: events go to the handler at once (also when no
/// request is in flight, e.g. the finishing steps and `done` after the
/// stream), answers to the session.
fn reader(mut input: impl Inbound, to_session: mpsc::Sender<Result<Incoming, Fault>>, on_event: OnEvent) {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        match input.read(Duration::from_millis(500)) {
            Ok(bytes) => buffer.extend_from_slice(&bytes),
            Err(f) if f.kind == Kind::Timeout => continue,
            Err(f) => {
                let _ = to_session.send(Err(f));
                return;
            }
        }
        while buffer.len() >= HEADER {
            let len = u32::from_le_bytes(buffer[4..8].try_into().unwrap()) as usize;
            let bad = Type::from(buffer[0]).is_none() || buffer[1..4] != [0, 0, 0] || len > MAX_PAYLOAD;
            if bad {
                let _ = to_session.send(Err(Fault::unexpected("the install system sent a malformed message")));
                return;
            }
            if buffer.len() < HEADER + len {
                break;
            }
            let kind = Type::from(buffer[0]).unwrap();
            let payload = buffer[HEADER..HEADER + len].to_vec();
            buffer.drain(..HEADER + len);
            let message = match kind {
                Type::Event => {
                    if let Ok(v) = serde_json::from_slice(&payload) {
                        (on_event.lock().unwrap())(&Event::parse(v));
                    }
                    Incoming::Alive
                }
                kind => Incoming::Answer(kind, payload),
            };
            if to_session.send(Ok(message)).is_err() {
                return;
            }
        }
    }
}

/// A connection to one install system.
pub struct Session {
    out: Box<dyn Outbound>,
    incoming: Receiver<Result<Incoming, Fault>>,
    on_event: OnEvent,
}

impl Session {
    pub(crate) fn new(out: impl Outbound + 'static, input: impl Inbound) -> Session {
        let (tx, rx) = mpsc::channel();
        let on_event: OnEvent = std::sync::Arc::new(std::sync::Mutex::new(Box::new(|_: &Event| {})));
        let handler = on_event.clone();
        std::thread::spawn(move || reader(input, tx, handler));
        Session { out: Box::new(out), incoming: rx, on_event }
    }

    /// Opens the receiver of the install system that `device` is, checking
    /// its ECID before claiming it.
    pub fn open(device: &Device) -> Result<Session, Failure> {
        let (out, input) = usb_link::open(device).map_err(|f| usb("opening the install system", f))?;
        Ok(Session::new(out, input))
    }

    /// Called for every event, also while a request waits for its answer.
    pub fn on_event(&mut self, f: impl FnMut(&Event) + Send + 'static) {
        *self.on_event.lock().unwrap() = Box::new(f);
    }

    fn send(&mut self, action: &'static str, kind: Type, payload: &[u8]) -> Result<(), Failure> {
        let message = frame(kind, payload);
        if !compat() {
            return self.out.write(&message, WRITE_TIMEOUT).map_err(|f| usb(action, f));
        }
        // Receivers before they read whole transfers: the header and the
        // payload as transfers of their own (functionfs dropped the rest of a
        // transfer longer than one read).
        let (header, body) = message.split_at(HEADER);
        self.out.write(header, WRITE_TIMEOUT).map_err(|f| usb(action, f))?;
        if !body.is_empty() {
            self.out.write(body, WRITE_TIMEOUT).map_err(|f| usb(action, f))?;
        }
        Ok(())
    }

    /// Waits for the answer to the request in flight (events reach the
    /// handler from the reader; each one counts as a sign of life).
    fn answer(&mut self, action: &'static str) -> Result<Value, Failure> {
        loop {
            match self.incoming.recv_timeout(SILENCE) {
                Ok(Ok(Incoming::Alive)) => continue,
                Ok(Ok(Incoming::Answer(Type::Ok, p))) => {
                    return serde_json::from_slice(&p).or_else(|_| if p.is_empty() { Ok(Value::Null) } else {
                        Err(usb(action, Fault::unexpected("the answer is not JSON")))
                    });
                }
                Ok(Ok(Incoming::Answer(_, p))) => {
                    let refusal = serde_json::from_slice(&p)
                        .unwrap_or(Refusal { code: "protocol".into(), message: "unreadable error".into() });
                    return Err(Failure::Refused(refusal));
                }
                Ok(Err(f)) => return Err(usb(action, f)),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(usb(action, Fault::new(Kind::Timeout, "the install system went silent")));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(usb(action, Fault::new(Kind::Disconnected, "the install system is gone")));
                }
            }
        }
    }

    fn request(&mut self, action: &'static str, kind: Type, body: &Value) -> Result<Value, Failure> {
        self.send(action, kind, body.to_string().as_bytes())?;
        self.answer(action)
    }

    /// The device's state. Checks the protocol version first.
    pub fn info(&mut self) -> Result<Info, Failure> {
        let value = self.request("info", Type::Info, &serde_json::json!({}))?;
        if let Some(v) = value.get("protocol").and_then(Value::as_u64).filter(|&v| v != VERSION) {
            return Err(Failure::Version(v));
        }
        serde_json::from_value(value).map_err(|e| usb("info", Fault::unexpected(e.to_string())))
    }

    /// The disk and its partitions, as `install_device.py inspect` reports them.
    pub fn inspect(&mut self) -> Result<Value, Failure> {
        self.request("inspect", Type::Inspect, &serde_json::json!({}))
    }

    /// Hands over the install plan; the device checks it against the live
    /// table, writes the new one and answers the new partitions' nodes
    /// (`{"root", "boot"}`).
    pub fn plan(&mut self, plan: &Value) -> Result<Value, Failure> {
        self.request("plan", Type::Plan, plan)
    }

    /// Stages a file under `name`; `sha256` is what the device checks.
    pub fn put(&mut self, name: &str, bytes: u64, sha256: &str, data: &mut dyn Read) -> Result<(), Failure> {
        let body = serde_json::json!({"name": name, "bytes": bytes, "sha256": sha256});
        self.transfer("put", Type::Put, &body, bytes, data)
    }

    /// Streams the root image. Restarting after a cut-off stream is sending it
    /// again from the start.
    pub fn stream(&mut self, bytes: u64, sha256: &str, data: &mut dyn Read) -> Result<(), Failure> {
        let body = serde_json::json!({"bytes": bytes, "sha256": sha256});
        self.transfer("stream", Type::Stream, &body, bytes, data)
    }

    fn transfer(&mut self, action: &'static str, kind: Type, body: &Value, bytes: u64, data: &mut dyn Read) -> Result<(), Failure> {
        self.request(action, kind, body)?;
        // Data in messages of the most the protocol allows; 64 KiB for older
        // receivers, whose framing broke on longer transfers.
        let data_chunk = if compat() { 64 << 10 } else { MAX_PAYLOAD };
        let mut chunk = vec![0u8; data_chunk];
        let mut left = bytes;
        while left > 0 {
            let want = (left as usize).min(data_chunk);
            data.read_exact(&mut chunk[..want]).map_err(|e| usb(action, Fault::new(Kind::Other, e.to_string())))?;
            self.send(action, Type::Data, &chunk[..want])?;
            left -= want as u64;
            // Events that arrived meanwhile; an early error ends the transfer.
            while let Ok(message) = self.incoming.try_recv() {
                match message {
                    Ok(Incoming::Alive) => {}
                    Ok(Incoming::Answer(Type::Ok, _)) => {
                        return Err(usb(action, Fault::unexpected("the device answered before the transfer ended")));
                    }
                    Ok(Incoming::Answer(_, p)) => {
                        let refusal = serde_json::from_slice(&p)
                            .unwrap_or(Refusal { code: "protocol".into(), message: "unreadable error".into() });
                        return Err(Failure::Refused(refusal));
                    }
                    Err(f) => return Err(usb(action, f)),
                }
            }
        }
        self.answer(action).map(drop)
    }
}

/// The real link: the receiver's vendor interface, found by class, subclass
/// and protocol, and its two bulk endpoints.
mod usb_link {
    use super::*;
    use crate::discovery::location;
    use nusb::descriptors::TransferType;
    use nusb::transfer::{Buffer, Bulk, In, Out};
    use nusb::MaybeFuture;

    pub struct UsbOut(nusb::Endpoint<Bulk, Out>, #[allow(dead_code)] nusb::Device);
    pub struct UsbIn(nusb::Endpoint<Bulk, In>);

    impl Outbound for UsbOut {
        fn write(&mut self, data: &[u8], timeout: Duration) -> Result<(), Fault> {
            let done = self.0.transfer_blocking(data.to_vec().into(), timeout);
            done.status?;
            if done.actual_len != data.len() {
                return Err(Fault::unexpected("incomplete write to the install system"));
            }
            Ok(())
        }
    }

    impl Inbound for UsbIn {
        fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, Fault> {
            // nusb (like the hardware) reads whole packets: the length must be
            // a multiple of the endpoint's packet size, else "invalid argument".
            let packet = self.0.max_packet_size().max(1);
            let len = (MAX_PAYLOAD + HEADER).div_ceil(packet) * packet;
            let done = self.0.transfer_blocking(Buffer::new(len), timeout);
            done.status?;
            Ok(done.buffer[..done.actual_len].to_vec())
        }
    }

    pub fn open(device: &Device) -> Result<(UsbOut, UsbIn), Fault> {
        let info = nusb::list_devices()
            .wait()?
            .find(|i| i.vendor_id() == VENDOR && i.product_id() == PRODUCT && location(i) == device.location)
            .ok_or_else(|| Fault::new(Kind::Disconnected, "the install system is gone"))?;
        let ecid = info.serial_number().map(crate::Identity::parse).and_then(|id| id.ecid);
        if ecid.is_none() || ecid != device.identity.ecid {
            return Err(Fault::new(Kind::WrongMode, "this install system belongs to another device"));
        }
        let opened = info.open().wait()?;
        let config = opened.active_configuration().map_err(|e| Fault::new(Kind::Other, e.to_string()))?;
        let mut found = None;
        for setting in config.interface_alt_settings() {
            if (setting.class(), setting.subclass(), setting.protocol()) != (CLASS, SUBCLASS, PROTOCOL) {
                continue;
            }
            let bulk = |dir_in: bool| {
                setting
                    .endpoints()
                    .find(|e| e.transfer_type() == TransferType::Bulk && (e.address() & 0x80 != 0) == dir_in)
                    .map(|e| e.address())
            };
            if let (Some(i), Some(o)) = (bulk(true), bulk(false)) {
                found = Some((setting.interface_number(), i, o));
                break;
            }
        }
        let (number, ep_in, ep_out) =
            found.ok_or_else(|| Fault::new(Kind::Unsupported, "the install system has no receiver interface"))?;
        // Just after a session ends its reader may still hold the interface
        // for a moment: try again for a few seconds.
        let mut tries = 0;
        let interface = loop {
            match opened.claim_interface(number).wait() {
                Ok(i) => break i,
                Err(_) if tries < 20 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e.into()),
            }
        };
        // Selecting alternate setting 0 starts a fresh session on the device:
        // anything half-sent before is dropped (install-protocol.md, Sessions).
        interface.set_alt_setting(0).wait()?;
        let input = interface.endpoint::<Bulk, In>(ep_in)?;
        let output = interface.endpoint::<Bulk, Out>(ep_out)?;
        Ok((UsbOut(output, opened), UsbIn(input)))
    }
}

#[cfg(test)]
mod tests;
