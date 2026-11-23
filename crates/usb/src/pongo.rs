//! The PongoOS shell over USB.
//!
//! Everything but file data goes through class control requests: request 2
//! reads whether the shell is busy, request 1 reads its output (or, as an
//! OUT request, announces an upload's length), request 3 sends a command
//! line and request 4 controls the transfer state. File data goes out on
//! bulk endpoint 2.
use crate::error::{During, Fault};
use crate::wire::{Deadline, Port, Request, Target, Usb};
use crate::{Device, Error, Mode, Sent};
use serde::Serialize;
use std::time::Duration;

const BULK_OUT: u8 = 0x02;
const TRANSFER: usize = 1 << 20;
const OUTPUT: u16 = 4096;

const READ_OUTPUT: u8 = 1;
const UPLOAD_LENGTH: u8 = 1;
const READ_BUSY: u8 = 2;
const COMMAND: u8 = 3;
const TRANSFER_STATE: u8 = 4;

/// What the shell had to say when asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Output {
    /// Whether a command is still running.
    pub busy: bool,
    /// Output printed since it was last read, up to 4096 bytes.
    pub text: String,
}

/// A device running PongoOS.
pub struct Pongo {
    usb: Box<dyn Usb>,
}

fn class(request: u8, value: u16) -> Request {
    Request {
        target: Target::ClassInterface,
        request,
        value,
    }
}

impl Pongo {
    pub fn open(device: &Device) -> Result<Pongo, Error> {
        let port =
            Port::open(device, Mode::Pongo, Some(BULK_OUT)).during("opening the Pongo device")?;
        Ok(Pongo {
            usb: Box::new(port),
        })
    }

    /// Reads whether the shell is busy and the output it has printed since
    /// the last read.
    pub fn output(&mut self, timeout: Duration) -> Result<Output, Error> {
        let deadline = Deadline::after(timeout);
        let mut read = || {
            let busy = self.busy(&deadline)?;
            let text = self
                .usb
                .control_in(class(READ_OUTPUT, 0), OUTPUT, deadline.left()?)?;
            Ok(Output {
                busy,
                text: String::from_utf8_lossy(&text).into_owned(),
            })
        };
        read().during("Pongo output")
    }

    /// Uploads a file into Pongo's load buffer. The shell must be idle.
    pub fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error> {
        self.send_image(image, &Deadline::after(timeout))
            .during("Pongo send")
    }

    /// Sends one command line. The shell must be idle. A command may start
    /// something else, so when the device stops answering during the final
    /// transfer the result is [`Sent::Unconfirmed`] rather than an error.
    pub fn command(&mut self, command: &str, timeout: Duration) -> Result<Sent, Error> {
        self.run(command, &Deadline::after(timeout))
            .during("Pongo command")
    }

    fn busy(&mut self, deadline: &Deadline) -> Result<bool, Fault> {
        let reply = self
            .usb
            .control_in(class(READ_BUSY, 0), 1, deadline.left()?)?;
        match reply.as_slice() {
            [state] => Ok(*state != 0),
            _ => Err(Fault::unexpected("the shell did not report its state")),
        }
    }

    fn expect_idle(&mut self, deadline: &Deadline) -> Result<(), Fault> {
        if self.busy(deadline)? {
            return Err(Fault::unexpected("the shell is busy with a command"));
        }
        Ok(())
    }

    fn send_image(&mut self, image: &[u8], deadline: &Deadline) -> Result<(), Fault> {
        let length = match u32::try_from(image.len()) {
            Ok(length) if length > 0 => length,
            Ok(_) => return Err(Fault::invalid("the image is empty")),
            Err(_) => return Err(Fault::invalid("the image is larger than 4 GiB")),
        };
        self.expect_idle(deadline)?;
        self.usb
            .control_out(class(TRANSFER_STATE, 0xffff), &[], deadline.left()?)?;
        self.usb.control_out(
            class(UPLOAD_LENGTH, 0),
            &length.to_le_bytes(),
            deadline.left()?,
        )?;
        for part in image.chunks(TRANSFER) {
            self.usb.bulk_out(part, deadline.left()?)?;
        }
        Ok(())
    }

    fn run(&mut self, command: &str, deadline: &Deadline) -> Result<Sent, Fault> {
        let printable = command.bytes().all(|b| (0x20..0x7f).contains(&b));
        if command.is_empty() || command.len() > 255 || !printable {
            return Err(Fault::invalid(
                "a command is one line of 1 to 255 printable ASCII characters",
            ));
        }
        self.expect_idle(deadline)?;
        self.usb
            .control_out(class(TRANSFER_STATE, 0xffff), &[], deadline.left()?)?;
        self.usb
            .control_out(class(TRANSFER_STATE, 1), &[], deadline.left()?)?;
        let line = format!("{command}\n");
        match self
            .usb
            .control_out(class(COMMAND, 0), line.as_bytes(), deadline.left()?)
        {
            Ok(()) => Ok(Sent::Acknowledged),
            Err(_) => Ok(Sent::Unconfirmed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::fake::{FakeUsb, Log, Op};
    use crate::Kind;

    const LONG: Duration = Duration::from_secs(5);

    fn pongo(fake: FakeUsb) -> (Pongo, Log<Op>) {
        let log = fake.log.clone();
        (
            Pongo {
                usb: Box::new(fake),
            },
            log,
        )
    }

    #[test]
    fn output_reads_state_then_text() {
        let (mut p, log) = pongo(FakeUsb::replying(&[&[1], b"pongoOS> "]));
        assert_eq!(
            p.output(LONG).unwrap(),
            Output {
                busy: true,
                text: "pongoOS> ".into()
            }
        );
        assert_eq!(
            *log.borrow(),
            vec![
                Op::In(class(READ_BUSY, 0), 1),
                Op::In(class(READ_OUTPUT, 0), OUTPUT)
            ]
        );
    }

    #[test]
    fn send_sequence() {
        let image = vec![3u8; TRANSFER + 5];
        let (mut p, log) = pongo(FakeUsb::replying(&[&[0]]));
        p.send(&image, LONG).unwrap();
        assert_eq!(
            *log.borrow(),
            vec![
                Op::In(class(READ_BUSY, 0), 1),
                Op::Out(class(TRANSFER_STATE, 0xffff), vec![]),
                Op::Out(class(UPLOAD_LENGTH, 0), vec![5, 0, 0x10, 0]),
                Op::Bulk(vec![3; TRANSFER]),
                Op::Bulk(vec![3; 5]),
            ]
        );
    }

    #[test]
    fn command_sequence() {
        let (mut p, log) = pongo(FakeUsb::replying(&[&[0]]));
        assert_eq!(p.command("bootm", LONG).unwrap(), Sent::Acknowledged);
        assert_eq!(
            *log.borrow(),
            vec![
                Op::In(class(READ_BUSY, 0), 1),
                Op::Out(class(TRANSFER_STATE, 0xffff), vec![]),
                Op::Out(class(TRANSFER_STATE, 1), vec![]),
                Op::Out(class(COMMAND, 0), b"bootm\n".to_vec()),
            ]
        );
    }

    #[test]
    fn command_the_device_does_not_answer_is_unconfirmed() {
        let mut fake = FakeUsb::replying(&[&[0]]);
        fake.fail_out = Some(COMMAND);
        let (mut p, _) = pongo(fake);
        assert_eq!(p.command("bootx", LONG).unwrap(), Sent::Unconfirmed);
    }

    #[test]
    fn busy_shell_gets_nothing_sent() {
        for act in [
            (|p: &mut Pongo| p.send(&[1], LONG).map(|_| ())) as fn(&mut Pongo) -> _,
            |p: &mut Pongo| p.command("help", LONG).map(|_| ()),
        ] {
            let (mut p, log) = pongo(FakeUsb::replying(&[&[1]]));
            assert_eq!(act(&mut p).unwrap_err().kind, Kind::Unexpected);
            assert_eq!(log.borrow().len(), 1);
        }
    }

    #[test]
    fn arguments_that_cannot_be_sent() {
        let (mut p, log) = pongo(FakeUsb::default());
        assert_eq!(p.send(&[], LONG).unwrap_err().kind, Kind::Invalid);
        assert_eq!(p.command("a\nb", LONG).unwrap_err().kind, Kind::Invalid);
        assert!(log.borrow().is_empty());
    }
}
