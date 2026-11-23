//! The USB operations the protocols are written against.
//!
//! [`Usb`] carries control transfers, bulk OUT and a reset; [`Serial`] is a
//! byte stream to a CDC serial function. The host implementations are
//! [`Port`] (nusb) and [`crate::serial_port::Tty`]. Tests put a fake that
//! records transfers and replays answers behind the same traits.
use crate::discovery::location;
use crate::error::{Fault, Kind};
use crate::{Device, Mode};
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, Out, Recipient};
use nusb::MaybeFuture;
use std::time::{Duration, Instant};

/// Who a control request is addressed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// `bmRequestType` 0x21 out, 0xa1 in.
    ClassInterface,
    /// 0x40 out, 0xc0 in.
    VendorDevice,
    /// 0x41 out.
    VendorInterface,
}

/// A control request. `wIndex` is zero in every protocol here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Request {
    pub target: Target,
    pub request: u8,
    pub value: u16,
}

pub(crate) trait Usb {
    fn control_out(
        &mut self,
        request: Request,
        data: &[u8],
        timeout: Duration,
    ) -> Result<(), Fault>;
    fn control_in(
        &mut self,
        request: Request,
        length: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, Fault>;
    /// Sends all of `data` on the protocol's bulk OUT endpoint.
    fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<(), Fault>;
    /// Resets the device's port. The device enumerates again afterwards, so
    /// this handle is of no further use.
    fn reset(&mut self) -> Result<(), Fault>;
}

pub(crate) trait Serial {
    /// Reads at least one byte.
    fn read(&mut self, data: &mut [u8], timeout: Duration) -> Result<usize, Fault>;
    /// Writes all of `data`.
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<(), Fault>;
}

/// The time one action may take, shared by the transfers it is made of.
pub(crate) struct Deadline(Instant);

impl Deadline {
    pub fn after(timeout: Duration) -> Deadline {
        Deadline(Instant::now() + timeout)
    }

    /// The time left for the next transfer.
    pub fn left(&self) -> Result<Duration, Fault> {
        let left = self.0.saturating_duration_since(Instant::now());
        // USB timeouts count milliseconds, and zero would mean no timeout.
        if left < Duration::from_millis(1) {
            return Err(Fault::new(Kind::Timeout, "timed out"));
        }
        Ok(left)
    }
}

/// Fails when the device is in another mode than the protocol talks to.
pub(crate) fn expect_mode(device: &Device, mode: Mode) -> Result<(), Fault> {
    if device.mode == mode {
        return Ok(());
    }
    Err(Fault::new(
        Kind::WrongMode,
        format!(
            "the device is in {} mode, not {}",
            device.mode.as_str(),
            mode.as_str()
        ),
    ))
}

/// An opened device on the real USB host.
pub(crate) struct Port {
    device: nusb::Device,
    bulk: Option<nusb::Endpoint<Bulk, Out>>,
}

impl Port {
    /// Opens the device at `device.location`, with the bulk OUT endpoint the
    /// protocol uses, if any.
    pub fn open(device: &Device, mode: Mode, bulk_out: Option<u8>) -> Result<Port, Fault> {
        expect_mode(device, mode)?;
        let info = nusb::list_devices()
            .wait()?
            .find(|info| {
                Mode::from_ids(info.vendor_id(), info.product_id()) == Some(mode)
                    && location(info) == device.location
            })
            .ok_or_else(|| {
                Fault::new(
                    Kind::Disconnected,
                    format!("no {} device at {}", mode.as_str(), device.location),
                )
            })?;
        let opened = info.open().wait()?;
        opened.set_configuration(1).wait()?;
        let bulk = match bulk_out {
            Some(address) => {
                let interface = opened.detach_and_claim_interface(0).wait()?;
                Some(interface.endpoint::<Bulk, Out>(address)?)
            }
            None => None,
        };
        Ok(Port {
            device: opened,
            bulk,
        })
    }
}

fn split(target: Target) -> (ControlType, Recipient) {
    match target {
        Target::ClassInterface => (ControlType::Class, Recipient::Interface),
        Target::VendorDevice => (ControlType::Vendor, Recipient::Device),
        Target::VendorInterface => (ControlType::Vendor, Recipient::Interface),
    }
}

impl Usb for Port {
    fn control_out(
        &mut self,
        request: Request,
        data: &[u8],
        timeout: Duration,
    ) -> Result<(), Fault> {
        let (control_type, recipient) = split(request.target);
        let transfer = ControlOut {
            control_type,
            recipient,
            request: request.request,
            value: request.value,
            index: 0,
            data,
        };
        Ok(self.device.control_out(transfer, timeout).wait()?)
    }

    fn control_in(
        &mut self,
        request: Request,
        length: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, Fault> {
        let (control_type, recipient) = split(request.target);
        let transfer = ControlIn {
            control_type,
            recipient,
            request: request.request,
            value: request.value,
            index: 0,
            length,
        };
        Ok(self.device.control_in(transfer, timeout).wait()?)
    }

    fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<(), Fault> {
        let endpoint = self
            .bulk
            .as_mut()
            .ok_or_else(|| Fault::new(Kind::Other, "this mode has no bulk endpoint"))?;
        let done = endpoint.transfer_blocking(data.to_vec().into(), timeout);
        done.status?;
        if done.actual_len != data.len() {
            return Err(Fault::unexpected(format!(
                "the device took {} of {} bytes",
                done.actual_len,
                data.len()
            )));
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Fault> {
        self.bulk = None;
        match self.device.reset().wait() {
            Ok(()) => Ok(()),
            // A device that already left has done what the reset asks for.
            Err(e) if e.kind() == nusb::ErrorKind::Disconnected => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Fakes for the protocol tests: they record what was sent and replay
/// scripted answers.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Op {
        Out(Request, Vec<u8>),
        In(Request, u16),
        Bulk(Vec<u8>),
        Reset,
    }

    /// What a test keeps to look at after the handle took the fake.
    pub type Log<T> = Rc<RefCell<Vec<T>>>;

    #[derive(Default)]
    pub struct FakeUsb {
        pub log: Log<Op>,
        /// Answers to control IN requests, in order.
        pub replies: VecDeque<Vec<u8>>,
        /// A control OUT request number that fails as a disconnect.
        pub fail_out: Option<u8>,
        /// A control transfer's zero-based place in the log, and its failure.
        pub fail_at: Option<(usize, Kind)>,
    }

    impl FakeUsb {
        fn transfer(&self) -> Result<(), Fault> {
            if let Some((at, kind)) = self.fail_at {
                if self.log.borrow().len() == at + 1 {
                    return Err(Fault::new(kind, "scripted transfer failure"));
                }
            }
            Ok(())
        }

        pub fn replying(replies: &[&[u8]]) -> FakeUsb {
            FakeUsb {
                replies: replies.iter().map(|r| r.to_vec()).collect(),
                ..FakeUsb::default()
            }
        }
    }

    impl Usb for FakeUsb {
        fn control_out(&mut self, request: Request, data: &[u8], _: Duration) -> Result<(), Fault> {
            self.log.borrow_mut().push(Op::Out(request, data.to_vec()));
            self.transfer()?;
            if self.fail_out == Some(request.request) {
                return Err(Fault::new(Kind::Disconnected, "the device disconnected"));
            }
            Ok(())
        }

        fn control_in(
            &mut self,
            request: Request,
            length: u16,
            _: Duration,
        ) -> Result<Vec<u8>, Fault> {
            self.log.borrow_mut().push(Op::In(request, length));
            self.transfer()?;
            self.replies
                .pop_front()
                .ok_or_else(|| Fault::new(Kind::Timeout, "timed out"))
        }

        fn bulk_out(&mut self, data: &[u8], _: Duration) -> Result<(), Fault> {
            self.log.borrow_mut().push(Op::Bulk(data.to_vec()));
            Ok(())
        }

        fn reset(&mut self) -> Result<(), Fault> {
            self.log.borrow_mut().push(Op::Reset);
            Ok(())
        }
    }

    #[derive(Default)]
    pub struct FakeSerial {
        /// Everything written to the device.
        pub written: Log<u8>,
        /// The bytes the device sends, in order.
        pub incoming: VecDeque<u8>,
    }

    impl Serial for FakeSerial {
        fn read(&mut self, data: &mut [u8], _: Duration) -> Result<usize, Fault> {
            let n = data.len().min(self.incoming.len());
            if n == 0 {
                return Err(Fault::new(Kind::Timeout, "timed out"));
            }
            for (slot, byte) in data.iter_mut().zip(self.incoming.drain(..n)) {
                *slot = byte;
            }
            Ok(n)
        }

        fn write(&mut self, data: &[u8], _: Duration) -> Result<(), Fault> {
            self.written.borrow_mut().extend_from_slice(data);
            Ok(())
        }
    }
}
