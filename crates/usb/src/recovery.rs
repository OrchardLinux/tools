//! iBoot's recovery mode.
//!
//! A file upload is announced with a vendor control request and then sent on
//! bulk endpoint 4 in 0x8000-byte transfers. Commands are NUL-terminated
//! strings in vendor control requests; iBoot answers `getenv` on the
//! matching IN request.
use crate::error::{During, Fault};
use crate::wire::{Deadline, Port, Request, Target, Usb};
use crate::{Device, Error, Mode, Sent};
use std::time::Duration;

const BULK_OUT: u8 = 0x04;
const TRANSFER: usize = 0x8000;

/// A device in recovery mode.
pub struct Recovery {
    usb: Box<dyn Usb>,
}

fn vendor(target: Target, request: u8) -> Request {
    Request {
        target,
        request,
        value: 0,
    }
}

/// The commands that make iBoot start something else. iBoot only acts on
/// them when they arrive as request 1, and it may be gone before it answers.
fn leaves(command: &str) -> bool {
    matches!(command, "go" | "bootx" | "reboot" | "memboot")
}

fn check_command(command: &str) -> Result<(), Fault> {
    let printable = command.bytes().all(|b| (0x20..0x7f).contains(&b));
    if command.is_empty() || command.len() > 255 || !printable {
        return Err(Fault::invalid(
            "a command is one line of 1 to 255 printable ASCII characters",
        ));
    }
    Ok(())
}

impl Recovery {
    pub fn open(device: &Device) -> Result<Recovery, Error> {
        let port = Port::open(device, Mode::Recovery, Some(BULK_OUT))
            .during("opening the recovery device")?;
        Ok(Recovery {
            usb: Box::new(port),
        })
    }

    /// Uploads a file into iBoot's load buffer. What happens to it is up to
    /// the command that follows.
    pub fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error> {
        self.send_image(image, &Deadline::after(timeout))
            .during("recovery send")
    }

    /// Runs one iBoot command. `go`, `bootx`, `reboot` and `memboot` make
    /// iBoot leave, so for them a failed transfer is [`Sent::Unconfirmed`]
    /// rather than an error.
    pub fn command(&mut self, command: &str, timeout: Duration) -> Result<Sent, Error> {
        self.run(command, timeout).during("recovery command")
    }

    /// Reads one iBoot environment variable.
    pub fn getenv(&mut self, name: &str, timeout: Duration) -> Result<String, Error> {
        self.read_variable(name, &Deadline::after(timeout))
            .during("recovery getenv")
    }

    fn send_image(&mut self, image: &[u8], deadline: &Deadline) -> Result<(), Fault> {
        if image.is_empty() {
            return Err(Fault::invalid("the image is empty"));
        }
        self.usb
            .control_out(vendor(Target::VendorInterface, 0), &[], deadline.left()?)?;
        for part in image.chunks(TRANSFER) {
            self.usb.bulk_out(part, deadline.left()?)?;
        }
        // A transfer that ends on a packet boundary needs an empty packet to
        // tell iBoot it is over.
        if image.len().is_multiple_of(512) {
            self.usb.bulk_out(&[], deadline.left()?)?;
        }
        Ok(())
    }

    fn run(&mut self, command: &str, timeout: Duration) -> Result<Sent, Fault> {
        check_command(command)?;
        let leaves = leaves(command);
        let data = [command.as_bytes(), &[0]].concat();
        let request = vendor(Target::VendorDevice, u8::from(leaves));
        match self.usb.control_out(request, &data, timeout) {
            Ok(()) => Ok(Sent::Acknowledged),
            Err(_) if leaves => Ok(Sent::Unconfirmed),
            Err(fault) => Err(fault),
        }
    }

    fn read_variable(&mut self, name: &str, deadline: &Deadline) -> Result<String, Fault> {
        let plain = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        if name.is_empty() || !name.bytes().all(plain) {
            return Err(Fault::invalid(
                "a variable name is letters, digits, - and _",
            ));
        }
        self.run(&format!("getenv {name}"), deadline.left()?)?;
        let reply = self
            .usb
            .control_in(vendor(Target::VendorDevice, 0), 255, deadline.left()?)?;
        let end = reply.iter().position(|&b| b == 0).unwrap_or(reply.len());
        Ok(String::from_utf8_lossy(&reply[..end]).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::fake::{FakeUsb, Log, Op};
    use crate::Kind;

    const LONG: Duration = Duration::from_secs(5);

    fn recovery(fake: FakeUsb) -> (Recovery, Log<Op>) {
        let log = fake.log.clone();
        (
            Recovery {
                usb: Box::new(fake),
            },
            log,
        )
    }

    #[test]
    fn send_sequence() {
        let image = vec![1u8; TRANSFER + 1000];
        let (mut r, log) = recovery(FakeUsb::default());
        r.send(&image, LONG).unwrap();
        assert_eq!(
            *log.borrow(),
            vec![
                Op::Out(vendor(Target::VendorInterface, 0), vec![]),
                Op::Bulk(vec![1; TRANSFER]),
                Op::Bulk(vec![1; 1000]),
            ]
        );
    }

    #[test]
    fn send_on_a_packet_boundary_ends_with_an_empty_packet() {
        let (mut r, log) = recovery(FakeUsb::default());
        r.send(&[1; 1024], LONG).unwrap();
        assert_eq!(log.borrow().last(), Some(&Op::Bulk(vec![])));
        assert_eq!(log.borrow().len(), 3);
    }

    #[test]
    fn empty_image_is_invalid_and_sends_nothing() {
        let (mut r, log) = recovery(FakeUsb::default());
        assert_eq!(r.send(&[], LONG).unwrap_err().kind, Kind::Invalid);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn ordinary_command() {
        let (mut r, log) = recovery(FakeUsb::default());
        assert_eq!(r.command("devicetree", LONG).unwrap(), Sent::Acknowledged);
        assert_eq!(
            *log.borrow(),
            vec![Op::Out(
                vendor(Target::VendorDevice, 0),
                b"devicetree\0".to_vec()
            )]
        );
    }

    #[test]
    fn leaving_command_uses_request_1_and_may_go_unanswered() {
        let (mut r, log) = recovery(FakeUsb {
            fail_out: Some(1),
            ..FakeUsb::default()
        });
        assert_eq!(r.command("bootx", LONG).unwrap(), Sent::Unconfirmed);
        assert_eq!(
            *log.borrow(),
            vec![Op::Out(
                vendor(Target::VendorDevice, 1),
                b"bootx\0".to_vec()
            )]
        );
    }

    #[test]
    fn ordinary_command_that_fails_is_an_error() {
        let (mut r, _) = recovery(FakeUsb {
            fail_out: Some(0),
            ..FakeUsb::default()
        });
        let e = r.command("setenv a b", LONG).unwrap_err();
        assert_eq!((e.action, e.kind), ("recovery command", Kind::Disconnected));
    }

    #[test]
    fn commands_that_cannot_be_sent() {
        let (mut r, log) = recovery(FakeUsb::default());
        for bad in ["", "two\nlines", &"x".repeat(256)] {
            assert_eq!(r.command(bad, LONG).unwrap_err().kind, Kind::Invalid);
        }
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn getenv_reads_the_answer() {
        let (mut r, log) = recovery(FakeUsb::replying(&[b"true\0junk"]));
        assert_eq!(r.getenv("auto-boot", LONG).unwrap(), "true");
        assert_eq!(
            *log.borrow(),
            vec![
                Op::Out(
                    vendor(Target::VendorDevice, 0),
                    b"getenv auto-boot\0".to_vec()
                ),
                Op::In(vendor(Target::VendorDevice, 0), 255),
            ]
        );
        assert_eq!(r.getenv("a b", LONG).unwrap_err().kind, Kind::Invalid);
    }
}
