//! The m1n1 proxy over its bulk pipe or first USB serial function.
//!
//! A request is a 64-byte frame: a 4-byte type, 56 bytes of arguments and a
//! checksum. The reply is 36 bytes: the same type, a status, 24 bytes of
//! result and a checksum. Memory contents travel as raw bytes right after
//! the request (write) or the reply (read). m1n1 also prints its console on
//! this stream, so anything that is not a reply is console text.
use crate::error::{During, Fault};
use crate::serial_port;
use crate::wire::{expect_mode, Deadline, Serial};
use crate::{Device, Error, Mode};
use std::time::Duration;

/// The proxy's control and data interface; the second function (2 and 3)
/// is m1n1's console only.
pub(crate) const FUNCTION: serial_port::Function = [0, 1];

const NOP: u32 = 0x00aa_55ff;
const CALL: u32 = 0x01aa_55ff;
const MEMREAD: u32 = 0x02aa_55ff;
const MEMWRITE: u32 = 0x03aa_55ff;
/// An event m1n1 sends unasked: type, 16-bit length and tag, data, checksum.
const EVENT: u32 = 0x05aa_55ff;

/// Memory moves in pieces of this size, each with its own checksum.
const PIECE: usize = 0x10000;
/// How much console text is kept between two [`Proxy::take_console`] calls.
const CONSOLE: usize = 0x10000;

/// A device running m1n1 with its proxy waiting.
pub struct Proxy {
    serial: Box<dyn Serial>,
    console: Vec<u8>,
    bulk: bool,
}

fn checksum(data: &[u8]) -> u32 {
    data.iter().fold(0xdead_beef_u32, |sum, byte| {
        sum.wrapping_mul(31337).wrapping_add(u32::from(byte ^ 0x5a))
    }) ^ 0xadde_dbad
}

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("4 bytes"))
}

fn le64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("8 bytes"))
}

impl Proxy {
    pub fn open(device: &Device) -> Result<Proxy, Error> {
        let open = || {
            expect_mode(device, Mode::M1n1)?;
            match crate::m1n1_bulk::Pipe::open(device)? {
                Some(pipe) => Ok((Box::new(pipe) as Box<dyn Serial>, true)),
                None => Ok((
                    Box::new(serial_port::open(device, FUNCTION)?) as Box<dyn Serial>,
                    false,
                )),
            }
        };
        let (serial, bulk) = open().during("opening the m1n1 proxy")?;
        Ok(Proxy {
            serial,
            console: Vec::new(),
            bulk,
        })
    }

    /// Whether this connection uses the direct USB bulk pipe.
    pub fn uses_bulk(&self) -> bool {
        self.bulk
    }

    /// Asks the proxy for an empty reply, which shows it is listening.
    pub fn ping(&mut self, timeout: Duration) -> Result<(), Error> {
        let deadline = Deadline::after(timeout);
        self.request(NOP, &[], &deadline)
            .and_then(|()| self.reply(NOP, &deadline))
            .map(|_| ())
            .during("m1n1 ping")
    }

    /// Runs one proxy operation with up to six arguments and returns its
    /// result.
    pub fn call(&mut self, operation: u64, args: &[u64], timeout: Duration) -> Result<u64, Error> {
        self.call_operation(operation, args, &Deadline::after(timeout))
            .during("m1n1 call")
    }

    /// Reads device memory.
    pub fn read(&mut self, address: u64, size: usize, timeout: Duration) -> Result<Vec<u8>, Error> {
        self.read_memory(address, size, &Deadline::after(timeout))
            .during("m1n1 read")
    }

    /// Writes device memory.
    pub fn write(&mut self, address: u64, data: &[u8], timeout: Duration) -> Result<(), Error> {
        self.write_memory(address, data, &Deadline::after(timeout))
            .during("m1n1 write")
    }

    /// The console text m1n1 printed between replies since the last call.
    pub fn take_console(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.console)
    }

    fn call_operation(
        &mut self,
        operation: u64,
        args: &[u64],
        deadline: &Deadline,
    ) -> Result<u64, Fault> {
        if args.len() > 6 {
            return Err(Fault::invalid("a proxy operation takes up to 6 arguments"));
        }
        let mut payload = operation.to_le_bytes().to_vec();
        for arg in args {
            payload.extend_from_slice(&arg.to_le_bytes());
        }
        self.request(CALL, &payload, deadline)?;
        let reply = self.reply(CALL, deadline)?;
        if le64(&reply[..8]) != operation {
            return Err(Fault::unexpected("the reply is for another operation"));
        }
        let status = le64(&reply[8..16]) as i64;
        if status != 0 {
            return Err(Fault::unexpected(format!(
                "the proxy answered operation 0x{operation:x} with status {status}"
            )));
        }
        Ok(le64(&reply[16..]))
    }

    fn read_memory(
        &mut self,
        address: u64,
        size: usize,
        deadline: &Deadline,
    ) -> Result<Vec<u8>, Fault> {
        if address.checked_add(size as u64).is_none() {
            return Err(Fault::invalid("the range runs past the end of memory"));
        }
        let mut memory = vec![0; size];
        for (index, piece) in memory.chunks_mut(PIECE).enumerate() {
            let mut payload = (address + (index * PIECE) as u64).to_le_bytes().to_vec();
            payload.extend_from_slice(&(piece.len() as u64).to_le_bytes());
            self.request(MEMREAD, &payload, deadline)?;
            let reply = self.reply(MEMREAD, deadline)?;
            self.read_exact(piece, deadline)?;
            if checksum(piece) != le32(&reply[..4]) {
                return Err(Fault::unexpected("the memory arrived damaged"));
            }
        }
        Ok(memory)
    }

    fn write_memory(
        &mut self,
        address: u64,
        data: &[u8],
        deadline: &Deadline,
    ) -> Result<(), Fault> {
        if address.checked_add(data.len() as u64).is_none() {
            return Err(Fault::invalid("the range runs past the end of memory"));
        }
        for (index, piece) in data.chunks(PIECE).enumerate() {
            let mut payload = (address + (index * PIECE) as u64).to_le_bytes().to_vec();
            payload.extend_from_slice(&(piece.len() as u64).to_le_bytes());
            payload.extend_from_slice(&checksum(piece).to_le_bytes());
            self.request(MEMWRITE, &payload, deadline)?;
            self.serial.write(piece, deadline.left()?)?;
            self.reply(MEMWRITE, deadline)?;
        }
        Ok(())
    }

    fn request(&mut self, kind: u32, payload: &[u8], deadline: &Deadline) -> Result<(), Fault> {
        let mut frame = kind.to_le_bytes().to_vec();
        frame.extend_from_slice(payload);
        frame.resize(60, 0);
        frame.extend_from_slice(&checksum(&frame).to_le_bytes());
        self.serial.write(&frame, deadline.left()?)
    }

    /// Reads up to the reply of type `kind` and returns its 24 result bytes.
    fn reply(&mut self, kind: u32, deadline: &Deadline) -> Result<[u8; 24], Fault> {
        loop {
            // Every frame type starts ff 55 aa; anything else is console.
            let mut head = [0; 4];
            self.read_exact(&mut head[..1], deadline)?;
            if head[0] != 0xff {
                self.keep_console(&head[..1]);
                continue;
            }
            self.read_exact(&mut head[1..], deadline)?;
            if head[1..3] != [0x55, 0xaa] {
                self.keep_console(&head);
                continue;
            }
            let found = le32(&head);
            if found == EVENT {
                self.skip_event(head, deadline)?;
                continue;
            }
            let mut frame = [0; 36];
            frame[..4].copy_from_slice(&head);
            self.read_exact(&mut frame[4..], deadline)?;
            if checksum(&frame[..32]) != le32(&frame[32..]) {
                return Err(Fault::unexpected("the reply arrived damaged"));
            }
            if found != kind {
                return Err(Fault::unexpected(format!(
                    "expected a reply of type 0x{kind:08x}, got 0x{found:08x}"
                )));
            }
            let status = le32(&frame[4..8]) as i32;
            if status != 0 {
                return Err(Fault::unexpected(format!(
                    "the proxy refused the request with status {status}"
                )));
            }
            return Ok(frame[8..32].try_into().expect("24 bytes"));
        }
    }

    fn skip_event(&mut self, head: [u8; 4], deadline: &Deadline) -> Result<(), Fault> {
        let mut event = head.to_vec();
        event.resize(8, 0);
        self.read_exact(&mut event[4..], deadline)?;
        let length = usize::from(u16::from_le_bytes([event[4], event[5]]));
        event.resize(8 + length + 4, 0);
        self.read_exact(&mut event[8..], deadline)?;
        if checksum(&event[..8 + length]) != le32(&event[8 + length..]) {
            return Err(Fault::unexpected("an event arrived damaged"));
        }
        Ok(())
    }

    fn read_exact(&mut self, mut data: &mut [u8], deadline: &Deadline) -> Result<(), Fault> {
        while !data.is_empty() {
            let n = self.serial.read(data, deadline.left()?)?;
            data = &mut data[n..];
        }
        Ok(())
    }

    fn keep_console(&mut self, text: &[u8]) {
        self.console.extend_from_slice(text);
        if self.console.len() > CONSOLE {
            let extra = self.console.len() - CONSOLE;
            self.console.drain(..extra);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::fake::{FakeSerial, Log};
    use crate::Kind;

    const LONG: Duration = Duration::from_secs(5);

    fn proxy(incoming: Vec<u8>) -> (Proxy, Log<u8>) {
        let fake = FakeSerial {
            incoming: incoming.into(),
            ..FakeSerial::default()
        };
        let written = fake.written.clone();
        (
            Proxy {
                serial: Box::new(fake),
                bulk: false,
                console: Vec::new(),
            },
            written,
        )
    }

    /// A reply frame as m1n1 builds it.
    fn reply(kind: u32, status: i32, result: &[u8]) -> Vec<u8> {
        let mut frame = kind.to_le_bytes().to_vec();
        frame.extend_from_slice(&status.to_le_bytes());
        frame.extend_from_slice(result);
        frame.resize(32, 0);
        frame.extend_from_slice(&checksum(&frame).to_le_bytes());
        frame
    }

    /// The request frame for `kind` with these 64-bit arguments.
    fn request(kind: u32, words: &[u64]) -> Vec<u8> {
        let mut frame = kind.to_le_bytes().to_vec();
        for word in words {
            frame.extend_from_slice(&word.to_le_bytes());
        }
        frame.resize(60, 0);
        frame.extend_from_slice(&checksum(&frame).to_le_bytes());
        frame
    }

    fn call_result(operation: u64, status: i64, value: u64) -> Vec<u8> {
        [
            operation.to_le_bytes(),
            status.to_le_bytes(),
            value.to_le_bytes(),
        ]
        .concat()
    }

    #[test]
    fn checksum_values() {
        assert_eq!(checksum(&[]), 0xdead_beef ^ 0xadde_dbad);
        // One byte: (0xdeadbeef * 31337 + (0 ^ 0x5a)) ^ 0xaddedbad.
        let one = 0xdead_beef_u32.wrapping_mul(31337).wrapping_add(0x5a) ^ 0xadde_dbad;
        assert_eq!(checksum(&[0]), one);
    }

    #[test]
    fn ping_frame() {
        let (mut p, written) = proxy(reply(NOP, 0, &[]));
        p.ping(LONG).unwrap();
        let sent = written.borrow().clone();
        assert_eq!(sent.len(), 64);
        assert_eq!(&sent[..4], &[0xff, 0x55, 0xaa, 0x00]);
        assert!(sent[4..60].iter().all(|&b| b == 0));
        assert_eq!(sent, request(NOP, &[]));
    }

    #[test]
    fn call_sends_operation_and_arguments() {
        let (mut p, written) = proxy(reply(CALL, 0, &call_result(0x700, 0, 42)));
        assert_eq!(p.call(0x700, &[1, 2], LONG).unwrap(), 42);
        assert_eq!(*written.borrow(), request(CALL, &[0x700, 1, 2]));
    }

    #[test]
    fn call_failures() {
        let (mut p, _) = proxy(reply(CALL, 0, &call_result(0x700, -1, 0)));
        assert_eq!(p.call(0x700, &[], LONG).unwrap_err().kind, Kind::Unexpected);
        let (mut p, _) = proxy(reply(CALL, 0, &call_result(0x701, 0, 0)));
        assert_eq!(p.call(0x700, &[], LONG).unwrap_err().kind, Kind::Unexpected);
        let (mut p, written) = proxy(vec![]);
        assert_eq!(p.call(1, &[0; 7], LONG).unwrap_err().kind, Kind::Invalid);
        assert!(written.borrow().is_empty());
    }

    #[test]
    fn console_text_and_events_around_a_reply() {
        let mut event = EVENT.to_le_bytes().to_vec();
        event.extend_from_slice(&[3, 0, 9, 0]);
        event.extend_from_slice(b"abc");
        event.extend_from_slice(&checksum(&event).to_le_bytes());
        let incoming = [b"boot\n".to_vec(), event, reply(NOP, 0, &[])].concat();
        let (mut p, _) = proxy(incoming);
        p.ping(LONG).unwrap();
        assert_eq!(p.take_console(), b"boot\n");
        assert!(p.take_console().is_empty());
    }

    #[test]
    fn damaged_refused_and_missing_replies() {
        let mut damaged = reply(NOP, 0, &[]);
        damaged[10] ^= 1;
        let (mut p, _) = proxy(damaged);
        assert_eq!(p.ping(LONG).unwrap_err().kind, Kind::Unexpected);

        let (mut p, _) = proxy(reply(NOP, -2, &[]));
        assert_eq!(p.ping(LONG).unwrap_err().kind, Kind::Unexpected);

        let (mut p, _) = proxy(reply(CALL, 0, &[]));
        assert_eq!(p.ping(LONG).unwrap_err().kind, Kind::Unexpected);

        let (mut p, _) = proxy(vec![]);
        let e = p.ping(LONG).unwrap_err();
        assert_eq!((e.action, e.kind), ("m1n1 ping", Kind::Timeout));
    }

    #[test]
    fn read_in_pieces() {
        let memory: Vec<u8> = (0..PIECE + 3).map(|i| (i * 7) as u8).collect();
        let (first, second) = memory.split_at(PIECE);
        let incoming = [
            reply(MEMREAD, 0, &checksum(first).to_le_bytes()),
            first.to_vec(),
            reply(MEMREAD, 0, &checksum(second).to_le_bytes()),
            second.to_vec(),
        ]
        .concat();
        let (mut p, written) = proxy(incoming);
        assert_eq!(p.read(0x8_0000_0000, memory.len(), LONG).unwrap(), memory);
        assert_eq!(
            *written.borrow(),
            [
                request(MEMREAD, &[0x8_0000_0000, PIECE as u64]),
                request(MEMREAD, &[0x8_0001_0000, 3]),
            ]
            .concat()
        );
    }

    #[test]
    fn read_checks_the_memory() {
        let incoming = [
            reply(MEMREAD, 0, &checksum(b"abcd").to_le_bytes()),
            b"abcx".to_vec(),
        ];
        let (mut p, _) = proxy(incoming.concat());
        assert_eq!(p.read(0, 4, LONG).unwrap_err().kind, Kind::Unexpected);
    }

    #[test]
    fn write_sends_header_then_data() {
        let data = b"hello".to_vec();
        let (mut p, written) = proxy(reply(MEMWRITE, 0, &[]));
        p.write(0x1000, &data, LONG).unwrap();
        let mut header = MEMWRITE.to_le_bytes().to_vec();
        header.extend_from_slice(&0x1000u64.to_le_bytes());
        header.extend_from_slice(&5u64.to_le_bytes());
        header.extend_from_slice(&checksum(&data).to_le_bytes());
        header.resize(60, 0);
        header.extend_from_slice(&checksum(&header).to_le_bytes());
        assert_eq!(*written.borrow(), [header, data].concat());
    }

    #[test]
    fn ranges_past_the_end_of_memory() {
        let (mut p, written) = proxy(vec![]);
        assert_eq!(p.read(u64::MAX, 2, LONG).unwrap_err().kind, Kind::Invalid);
        assert_eq!(
            p.write(u64::MAX, &[1, 2], LONG).unwrap_err().kind,
            Kind::Invalid
        );
        assert!(written.borrow().is_empty());
    }
}
