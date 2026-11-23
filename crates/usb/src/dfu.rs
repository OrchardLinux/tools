//! USB DFU as the boot ROM speaks it.
//!
//! An image goes to the device in DNLOAD control transfers of 0x800 bytes,
//! each followed by GETSTATUS. The last block carries the 16-byte DFU suffix
//! with its CRC. An empty DNLOAD, two GETSTATUS and a USB reset then make
//! the boot ROM run the image.
use crate::error::{During, Fault};
use crate::wire::{Deadline, Port, Request, Target, Usb};
use crate::{Device, Error, Kind, Mode, Sent};
use serde::Serialize;
use std::time::Duration;

const BLOCK: usize = 0x800;

/// The payload that receives a raw image on a pwned boot ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pwned {
    /// Project Cascadia's `lib/appleusb.py::boot_pwned` (pico-checkm8).
    Checkm8,
    /// Project Cascadia's `devices/a13/usbliter8ctl::do_boot`.
    Usbliter8,
}

const DNLOAD: u8 = 1;
const UPLOAD: u8 = 2;
const GETSTATUS: u8 = 3;
const CLRSTATUS: u8 = 4;
const GETSTATE: u8 = 5;
const ABORT: u8 = 6;

/// The checkm8 payload's boot request is UPLOAD with this value.
const CHECKM8_BOOT: u16 = 0xffff;
/// usbliter8's own boot request.
const USBLITER8_BOOT: u8 = 8;
/// usbliter8ctl sends this after its boot request and calls it DFU_ABORT,
/// but standard DFU's ABORT is 6. The wire value is what counts.
const USBLITER8_FINISH: u8 = 4;

/// Timeouts of the pwned-boot requests, as Cascadia's tools set them
/// (`boot_pwned`, `usbliter8ctl` at 091d441): the empty download, status
/// reads and usbliter8's requests 100 ms, each image block and checkm8's
/// boot request 5 s after checkm8 and 1 s after usbliter8.
const PWNED_REQUEST: Duration = Duration::from_millis(100);
const CHECKM8_BLOCK: Duration = Duration::from_secs(5);
const USBLITER8_BLOCK: Duration = Duration::from_secs(1);

const IDLE: u8 = 2;
const DOWNLOAD_BUSY: u8 = 4;
const DOWNLOAD_IDLE: u8 = 5;
const ERROR: u8 = 10;

/// The DFU file suffix before its CRC: device and product 0xffff, vendor
/// 0x05ac, DFU version 1.00, "UFD", length 16.
const SUFFIX: [u8; 12] = [
    0xff, 0xff, 0xff, 0xff, 0xac, 0x05, 0x00, 0x01, 0x55, 0x46, 0x44, 0x10,
];

/// The reply to a request that starts code, or that an exploit payload
/// answers, can be lost: the device leaves or the payload does not answer.
fn reply_may_be_lost<T>(reply: Result<T, Fault>) -> Result<Sent, Fault> {
    match reply {
        Ok(_) => Ok(Sent::Acknowledged),
        Err(e) if matches!(e.kind, Kind::Timeout | Kind::Stall | Kind::Disconnected) => {
            Ok(Sent::Unconfirmed)
        }
        Err(e) => Err(e),
    }
}

/// The answer to GETSTATUS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// The DFU status code; zero is OK.
    pub status: u8,
    /// The DFU state number.
    pub state: u8,
    /// The state's name in the DFU specification.
    pub state_name: &'static str,
    /// How long the device asks the host to wait before the next request.
    pub poll_timeout_ms: u32,
}

fn state_name(state: u8) -> &'static str {
    match state {
        0 => "appIDLE",
        1 => "appDETACH",
        2 => "dfuIDLE",
        3 => "dfuDNLOAD-SYNC",
        4 => "dfuDNBUSY",
        5 => "dfuDNLOAD-IDLE",
        6 => "dfuMANIFEST-SYNC",
        7 => "dfuMANIFEST",
        8 => "dfuMANIFEST-WAIT-RESET",
        9 => "dfuUPLOAD-IDLE",
        10 => "dfuERROR",
        _ => "unknown",
    }
}

/// A device in DFU mode.
pub struct Dfu {
    usb: Box<dyn Usb>,
}

fn class(request: u8, value: u16) -> Request {
    Request {
        target: Target::ClassInterface,
        request,
        value,
    }
}

impl Dfu {
    pub fn open(device: &Device) -> Result<Dfu, Error> {
        let port = Port::open(device, Mode::Dfu, None).during("opening the DFU device")?;
        Ok(Dfu {
            usb: Box::new(port),
        })
    }

    /// Reads the status and state (GETSTATUS).
    pub fn status(&mut self, timeout: Duration) -> Result<Status, Error> {
        self.get_status(timeout).during("DFU status")
    }

    /// Clears an error status (CLRSTATUS), which returns the device to idle.
    pub fn clear_status(&mut self, timeout: Duration) -> Result<(), Error> {
        self.usb
            .control_out(class(CLRSTATUS, 0), &[], timeout)
            .during("DFU clear status")
    }

    /// Abandons a transfer in progress (ABORT), which returns the device to
    /// idle.
    pub fn abort(&mut self, timeout: Duration) -> Result<(), Error> {
        self.usb
            .control_out(class(ABORT, 0), &[], timeout)
            .during("DFU abort")
    }

    /// Resets the device's USB port. The device enumerates again.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.usb.reset().during("DFU reset")
    }

    /// Sends an image and makes the boot ROM run it. The device must be
    /// idle. It resets at the end and enumerates again as whatever the image
    /// is.
    pub fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error> {
        self.send_image(image, &Deadline::after(timeout))
            .during("DFU send")
    }

    /// Uploads a raw image and asks the exploit payload to start it. The
    /// caller selects the payload from the device's marker. A boot request
    /// may lose its reply as the image starts; the caller must then wait
    /// for the new program rather than repeat the upload.
    pub fn start_pwned(
        &mut self,
        payload: Pwned,
        image: &[u8],
        timeout: Duration,
    ) -> Result<Sent, Error> {
        let deadline = Deadline::after(timeout);
        if image.is_empty() {
            return Err(Fault::invalid("the image is empty"))
                .during("DFU start on a pwned boot ROM");
        }
        match payload {
            Pwned::Checkm8 => self.start_checkm8(image, &deadline),
            Pwned::Usbliter8 => self.start_usbliter8(image, &deadline),
        }
        .during("DFU start on a pwned boot ROM")
    }

    /// Cascadia's `boot_pwned`: a 16-byte zero download and an empty one
    /// set the payload up, the image follows in plain downloads, and UPLOAD
    /// with value 0xffff jumps to it.
    fn start_checkm8(&mut self, image: &[u8], deadline: &Deadline) -> Result<Sent, Fault> {
        self.usb
            .control_out(class(DNLOAD, 0), &[0; 16], deadline.left()?)?;
        self.usb
            .control_out(class(DNLOAD, 0), &[], deadline.left()?.min(PWNED_REQUEST))?;
        for _ in 0..2 {
            // The payload's setup can leave these reads unanswered.
            reply_may_be_lost(self.usb.control_in(
                class(GETSTATUS, 0),
                6,
                deadline.left()?.min(PWNED_REQUEST),
            ))?;
        }
        self.download_raw(image, deadline, CHECKM8_BLOCK)?;
        let boot = self.usb.control_in(
            class(UPLOAD, CHECKM8_BOOT),
            0,
            deadline.left()?.min(CHECKM8_BLOCK),
        );
        reply_may_be_lost(boot.and_then(|bytes| match bytes.is_empty() {
            true => Ok(()),
            false => Err(Fault::unexpected("the boot request returned data")),
        }))
    }

    /// usbliter8ctl's `do_boot`: the image in plain downloads, an empty
    /// download, then usbliter8's boot request and its closing request.
    fn start_usbliter8(&mut self, image: &[u8], deadline: &Deadline) -> Result<Sent, Fault> {
        self.download_raw(image, deadline, USBLITER8_BLOCK)?;
        self.usb
            .control_out(class(DNLOAD, 0), &[], deadline.left()?.min(PWNED_REQUEST))?;
        let boot = self.usb.control_out(
            class(USBLITER8_BOOT, 0),
            &[],
            deadline.left()?.min(PWNED_REQUEST),
        );
        if reply_may_be_lost(boot)? == Sent::Unconfirmed {
            return Ok(Sent::Unconfirmed);
        }
        reply_may_be_lost(self.usb.control_out(
            class(USBLITER8_FINISH, 0),
            &[],
            deadline.left()?.min(PWNED_REQUEST),
        ))
    }

    /// A raw image in downloads of one block each, block number zero, with
    /// no status reads between them.
    fn download_raw(
        &mut self,
        image: &[u8],
        deadline: &Deadline,
        per_block: Duration,
    ) -> Result<(), Fault> {
        for block in image.chunks(BLOCK) {
            self.usb
                .control_out(class(DNLOAD, 0), block, deadline.left()?.min(per_block))?;
        }
        Ok(())
    }

    fn get_status(&mut self, timeout: Duration) -> Result<Status, Fault> {
        let reply = self.usb.control_in(class(GETSTATUS, 0), 6, timeout)?;
        let &[status, t0, t1, t2, state, _string] = reply.as_slice() else {
            return Err(Fault::unexpected(format!(
                "GETSTATUS returned {} bytes, not 6",
                reply.len()
            )));
        };
        Ok(Status {
            status,
            state,
            state_name: state_name(state),
            poll_timeout_ms: u32::from_le_bytes([t0, t1, t2, 0]),
        })
    }

    /// GETSTATUS that fails when the device reports an error.
    fn good_status(&mut self, deadline: &Deadline) -> Result<Status, Fault> {
        let status = self.get_status(deadline.left()?)?;
        if status.status != 0 || status.state == ERROR {
            return Err(Fault::unexpected(format!(
                "the device reports status {} in state {} ({})",
                status.status, status.state, status.state_name
            )));
        }
        Ok(status)
    }

    fn send_image(&mut self, image: &[u8], deadline: &Deadline) -> Result<(), Fault> {
        if image.is_empty() {
            return Err(Fault::invalid("the image is empty"));
        }
        let blocks = image.chunks(BLOCK).count();
        // Block numbers are 16 bits, and the closing request takes one more.
        let Ok(closing) = u16::try_from(blocks) else {
            return Err(Fault::invalid("the image is too large for DFU"));
        };

        let state = self
            .usb
            .control_in(class(GETSTATE, 0), 1, deadline.left()?)?;
        match state.as_slice() {
            [IDLE] => {}
            [other] => {
                return Err(Fault::unexpected(format!(
                    "the device is in state {other} ({}), not idle",
                    state_name(*other)
                )))
            }
            _ => return Err(Fault::unexpected("GETSTATE returned no state")),
        }

        let mut crc = crc32fast::Hasher::new();
        crc.update(image);
        crc.update(&SUFFIX);
        // The suffix carries the CRC register as it stands, not inverted.
        let mut suffix = SUFFIX.to_vec();
        suffix.extend_from_slice(&(!crc.finalize()).to_le_bytes());

        for (number, block) in image.chunks(BLOCK).enumerate() {
            let number = number as u16;
            if number + 1 < closing {
                self.download(number, block, deadline)?;
            } else if block.len() + suffix.len() <= BLOCK {
                self.download(number, &[block, &suffix].concat(), deadline)?;
            } else {
                // No room left in the last block: the suffix follows alone,
                // under the same block number.
                self.download(number, block, deadline)?;
                self.download(number, &suffix, deadline)?;
            }
        }

        self.usb
            .control_out(class(DNLOAD, closing), &[], deadline.left()?)?;
        for _ in 0..2 {
            self.good_status(deadline)?;
        }
        self.usb.reset()
    }

    /// One DNLOAD, then GETSTATUS until the device has taken the block.
    fn download(&mut self, number: u16, data: &[u8], deadline: &Deadline) -> Result<(), Fault> {
        self.usb
            .control_out(class(DNLOAD, number), data, deadline.left()?)?;
        loop {
            let status = self.good_status(deadline)?;
            match status.state {
                DOWNLOAD_IDLE => return Ok(()),
                DOWNLOAD_BUSY => {
                    // The DFU way of saying "ask again in this many ms".
                    let wait = Duration::from_millis(status.poll_timeout_ms.clamp(10, 1000).into());
                    std::thread::sleep(wait.min(deadline.left()?));
                }
                other => {
                    return Err(Fault::unexpected(format!(
                        "the device is in state {other} ({}) after block {number}",
                        status.state_name
                    )))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::fake::{FakeUsb, Log, Op};
    use crate::Kind;

    const LONG: Duration = Duration::from_secs(5);
    const TAKEN: &[u8] = &[0, 0, 0, 0, DOWNLOAD_IDLE, 0];

    fn dfu(replies: &[&[u8]]) -> (Dfu, Log<Op>) {
        let fake = FakeUsb::replying(replies);
        let log = fake.log.clone();
        (
            Dfu {
                usb: Box::new(fake),
            },
            log,
        )
    }

    /// The CRC as libirecovery computes it: table-driven, register not
    /// inverted at the end.
    fn reference_crc(data: &[u8]) -> u32 {
        let mut register = 0xffff_ffffu32;
        for &byte in data {
            register ^= u32::from(byte);
            for _ in 0..8 {
                register = if register & 1 != 0 {
                    0xedb8_8320 ^ (register >> 1)
                } else {
                    register >> 1
                };
            }
        }
        register
    }

    fn downloads(log: &Log<Op>) -> Vec<(u16, Vec<u8>)> {
        log.borrow()
            .iter()
            .filter_map(|op| match op {
                Op::Out(r, data) if r.request == DNLOAD => Some((r.value, data.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn checkm8_boot_matches_cascadias_boot_pwned() {
        let image = vec![0xa5; BLOCK + 7];
        let (mut dfu, log) = dfu(&[TAKEN, TAKEN, &[]]);
        assert_eq!(
            dfu.start_pwned(Pwned::Checkm8, &image, LONG).unwrap(),
            Sent::Acknowledged
        );
        assert_eq!(
            *log.borrow(),
            [
                Op::Out(class(DNLOAD, 0), vec![0; 16]),
                Op::Out(class(DNLOAD, 0), vec![]),
                Op::In(class(GETSTATUS, 0), 6),
                Op::In(class(GETSTATUS, 0), 6),
                Op::Out(class(DNLOAD, 0), image[..BLOCK].to_vec()),
                Op::Out(class(DNLOAD, 0), image[BLOCK..].to_vec()),
                Op::In(class(UPLOAD, CHECKM8_BOOT), 0),
            ]
        );
    }

    #[test]
    fn usbliter8_boot_matches_cascadias_control_tool() {
        let image = vec![0x5a; BLOCK * 2];
        let (mut dfu, log) = dfu(&[]);
        assert_eq!(
            dfu.start_pwned(Pwned::Usbliter8, &image, LONG).unwrap(),
            Sent::Acknowledged
        );
        assert_eq!(
            *log.borrow(),
            [
                Op::Out(class(DNLOAD, 0), image[..BLOCK].to_vec()),
                Op::Out(class(DNLOAD, 0), image[BLOCK..].to_vec()),
                Op::Out(class(DNLOAD, 0), vec![]),
                Op::Out(class(USBLITER8_BOOT, 0), vec![]),
                Op::Out(class(USBLITER8_FINISH, 0), vec![]),
            ]
        );
    }

    #[test]
    fn raw_starts_reject_empty_images_and_expired_deadlines_before_usb() {
        for payload in [Pwned::Checkm8, Pwned::Usbliter8] {
            let (mut dfu, log) = dfu(&[]);
            assert_eq!(
                dfu.start_pwned(payload, &[], LONG).unwrap_err().kind,
                Kind::Invalid
            );
            assert_eq!(
                dfu.start_pwned(payload, &[1], Duration::ZERO)
                    .unwrap_err()
                    .kind,
                Kind::Timeout
            );
            assert!(log.borrow().is_empty());
        }
    }

    #[test]
    fn a_raw_upload_failure_never_sends_the_boot_request() {
        for payload in [Pwned::Checkm8, Pwned::Usbliter8] {
            let fake = FakeUsb {
                fail_out: Some(DNLOAD),
                ..FakeUsb::default()
            };
            let log = fake.log.clone();
            let mut dfu = Dfu {
                usb: Box::new(fake),
            };
            assert_eq!(
                dfu.start_pwned(payload, &[1], LONG).unwrap_err().kind,
                Kind::Disconnected
            );
            assert_eq!(log.borrow().len(), 1);
        }
    }

    #[test]
    fn partial_uploads_stop_at_the_failed_transfer() {
        for (payload, at) in [
            (Pwned::Checkm8, 5),
            (Pwned::Usbliter8, 1),
            (Pwned::Usbliter8, 2),
        ] {
            let fake = FakeUsb {
                fail_at: Some((at, Kind::Timeout)),
                ..FakeUsb::replying(&[TAKEN, TAKEN])
            };
            let log = fake.log.clone();
            let mut dfu = Dfu {
                usb: Box::new(fake),
            };
            let e = dfu
                .start_pwned(payload, &vec![1; BLOCK + 1], LONG)
                .unwrap_err();
            assert_eq!(e.kind, Kind::Timeout);
            assert_eq!(log.borrow().len(), at + 1);
            assert!(matches!(log.borrow().last(), Some(Op::Out(r, _)) if r.request == DNLOAD));
        }
    }

    #[test]
    fn a_host_failure_during_bootstrap_stops_before_the_image() {
        let fake = FakeUsb {
            fail_at: Some((2, Kind::Denied)),
            ..FakeUsb::default()
        };
        let log = fake.log.clone();
        let mut dfu = Dfu {
            usb: Box::new(fake),
        };
        assert_eq!(
            dfu.start_pwned(Pwned::Checkm8, &[1], LONG)
                .unwrap_err()
                .kind,
            Kind::Denied
        );
        assert_eq!(log.borrow().len(), 3);
    }

    #[test]
    fn boot_requests_may_leave_without_a_reply() {
        // checkm8's bootstrap status reads and final boot request time out.
        let (mut dfu, log) = dfu(&[]);
        assert_eq!(
            dfu.start_pwned(Pwned::Checkm8, &[1], LONG).unwrap(),
            Sent::Unconfirmed
        );
        assert_eq!(
            log.borrow().last(),
            Some(&Op::In(class(UPLOAD, CHECKM8_BOOT), 0))
        );

        for request in [USBLITER8_BOOT, USBLITER8_FINISH] {
            let fake = FakeUsb {
                fail_out: Some(request),
                ..FakeUsb::default()
            };
            let log = fake.log.clone();
            let mut dfu = Dfu {
                usb: Box::new(fake),
            };
            assert_eq!(
                dfu.start_pwned(Pwned::Usbliter8, &[1], LONG).unwrap(),
                Sent::Unconfirmed
            );
            assert_eq!(
                log.borrow().last(),
                Some(&Op::Out(class(request, 0), vec![]))
            );
        }
    }

    #[test]
    fn only_transfer_failures_can_be_unconfirmed() {
        for kind in [Kind::Timeout, Kind::Stall, Kind::Disconnected] {
            assert_eq!(
                reply_may_be_lost::<()>(Err(Fault::new(kind, "test"))).unwrap(),
                Sent::Unconfirmed
            );
        }
        for kind in [Kind::Denied, Kind::Busy, Kind::Unexpected, Kind::Other] {
            assert_eq!(
                reply_may_be_lost::<()>(Err(Fault::new(kind, "test")))
                    .unwrap_err()
                    .kind,
                kind
            );
        }
        let (mut dfu, _) = dfu(&[TAKEN, TAKEN, &[1]]);
        assert_eq!(
            dfu.start_pwned(Pwned::Checkm8, &[1], LONG)
                .unwrap_err()
                .kind,
            Kind::Unexpected
        );
    }

    #[test]
    fn status_reply() {
        let (mut dfu, log) = dfu(&[&[0, 0x10, 0x02, 0, 8, 0]]);
        assert_eq!(
            dfu.status(LONG).unwrap(),
            Status {
                status: 0,
                state: 8,
                state_name: "dfuMANIFEST-WAIT-RESET",
                poll_timeout_ms: 0x210,
            }
        );
        assert_eq!(*log.borrow(), vec![Op::In(class(GETSTATUS, 0), 6)]);
    }

    #[test]
    fn short_status_is_unexpected() {
        let (mut dfu, _) = dfu(&[&[0, 0]]);
        let e = dfu.status(LONG).unwrap_err();
        assert_eq!((e.action, e.kind), ("DFU status", Kind::Unexpected));
    }

    #[test]
    fn single_requests() {
        let (mut dfu, log) = dfu(&[]);
        dfu.clear_status(LONG).unwrap();
        dfu.abort(LONG).unwrap();
        dfu.reset().unwrap();
        assert_eq!(
            *log.borrow(),
            vec![
                Op::Out(class(CLRSTATUS, 0), vec![]),
                Op::Out(class(ABORT, 0), vec![]),
                Op::Reset,
            ]
        );
    }

    #[test]
    fn send_sequence() {
        let image: Vec<u8> = (0..BLOCK * 2 + 100).map(|i| i as u8).collect();
        let (mut dfu, log) = dfu(&[&[IDLE], TAKEN, TAKEN, TAKEN, TAKEN, TAKEN]);
        dfu.send(&image, LONG).unwrap();

        assert_eq!(log.borrow()[0], Op::In(class(GETSTATE, 0), 1));
        let blocks = downloads(&log);
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0], (0, image[..BLOCK].to_vec()));
        assert_eq!(blocks[1], (1, image[BLOCK..BLOCK * 2].to_vec()));
        let (number, last) = &blocks[2];
        assert_eq!(*number, 2);
        assert_eq!(&last[..100], &image[BLOCK * 2..]);
        assert_eq!(&last[100..112], &SUFFIX);
        let crc = reference_crc(&[&image[..], &SUFFIX].concat());
        assert_eq!(&last[112..], &crc.to_le_bytes());
        assert_eq!(blocks[3], (3, vec![]));

        // A status after each block and two after the closing request.
        let statuses = log
            .borrow()
            .iter()
            .filter(|op| **op == Op::In(class(GETSTATUS, 0), 6))
            .count();
        assert_eq!(statuses, 5);
        assert_eq!(log.borrow().last(), Some(&Op::Reset));
    }

    #[test]
    fn suffix_without_room_follows_alone() {
        let image = vec![0xa5; BLOCK - 8];
        let (mut dfu, log) = dfu(&[&[IDLE], TAKEN, TAKEN, TAKEN, TAKEN]);
        dfu.send(&image, LONG).unwrap();
        let blocks = downloads(&log);
        assert_eq!(blocks[0], (0, image));
        assert_eq!((blocks[1].0, blocks[1].1.len()), (0, 16));
        assert_eq!(blocks[2], (1, vec![]));
    }

    #[test]
    fn busy_device_is_asked_again() {
        let busy: &[u8] = &[0, 1, 0, 0, DOWNLOAD_BUSY, 0];
        let (mut dfu, log) = dfu(&[&[IDLE], busy, TAKEN, TAKEN, TAKEN]);
        dfu.send(&[7; 10], LONG).unwrap();
        assert_eq!(log.borrow().last(), Some(&Op::Reset));
    }

    #[test]
    fn send_needs_an_idle_device_and_leaves_it_alone() {
        let (mut dfu, log) = dfu(&[&[8]]);
        let e = dfu.send(&[1, 2, 3], LONG).unwrap_err();
        assert_eq!((e.action, e.kind), ("DFU send", Kind::Unexpected));
        assert_eq!(log.borrow().len(), 1);
    }

    #[test]
    fn error_status_stops_the_send() {
        let failed: &[u8] = &[0, 0, 0, 0, ERROR, 0];
        let (mut dfu, log) = dfu(&[&[IDLE], failed]);
        assert_eq!(dfu.send(&[1], LONG).unwrap_err().kind, Kind::Unexpected);
        assert!(!log.borrow().contains(&Op::Reset));
    }

    #[test]
    fn empty_image_is_invalid_and_sends_nothing() {
        let (mut dfu, log) = dfu(&[]);
        assert_eq!(dfu.send(&[], LONG).unwrap_err().kind, Kind::Invalid);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn no_time_left_is_a_timeout_before_any_transfer() {
        let (mut dfu, log) = dfu(&[]);
        let e = dfu.send(&[1], Duration::ZERO).unwrap_err();
        assert_eq!(e.kind, Kind::Timeout);
        assert!(log.borrow().is_empty());
    }
}
