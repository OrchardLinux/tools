//! Direct access to m1n1's vendor-class proxy pipe; CDC remains available.
use crate::discovery::location;
use crate::error::{Fault, Kind};
use crate::wire::{Deadline, Serial};
use crate::{Device, Mode};
use nusb::transfer::{Bulk, ControlOut, ControlType, In, Out, Recipient};
use nusb::MaybeFuture;
use std::time::Duration;

pub(crate) struct Pipe {
    _device: nusb::Device,
    input: nusb::Endpoint<Bulk, In>,
    output: nusb::Endpoint<Bulk, Out>,
    pending: Vec<u8>,
    position: usize,
}

impl Pipe {
    pub fn open(device: &Device) -> Result<Option<Self>, Fault> {
        let info = nusb::list_devices()
            .wait()?
            .find(|info| {
                Mode::from_ids(info.vendor_id(), info.product_id()) == Some(Mode::M1n1)
                    && location(info) == device.location
            })
            .ok_or_else(|| Fault::new(Kind::Disconnected, "m1n1 disconnected"))?;
        if !info
            .interfaces()
            .any(|i| i.interface_number() == 3 && i.class() == 0xff)
        {
            return Ok(None);
        }
        let opened = info.open().wait()?;
        let interface = opened.claim_interface(3).wait()?;
        let input = interface.endpoint::<Bulk, In>(0x86)?;
        let output = interface.endpoint::<Bulk, Out>(0x05)?;
        opened
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: 0x22,
                    value: 3,
                    index: 2,
                    data: &[],
                },
                Duration::from_secs(1),
            )
            .wait()?;
        Ok(Some(Self {
            _device: opened,
            input,
            output,
            pending: Vec::new(),
            position: 0,
        }))
    }
}

impl Serial for Pipe {
    fn read(&mut self, data: &mut [u8], timeout: Duration) -> Result<usize, Fault> {
        let deadline = Deadline::after(timeout);
        while self.position == self.pending.len() {
            let done = self
                .input
                .transfer_blocking(nusb::transfer::Buffer::new(0x10000), deadline.left()?);
            done.status?;
            self.pending = done.buffer[..done.actual_len].to_vec();
            self.position = 0;
        }
        let n = data.len().min(self.pending.len() - self.position);
        data[..n].copy_from_slice(&self.pending[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }

    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<(), Fault> {
        let done = self.output.transfer_blocking(data.to_vec().into(), timeout);
        done.status?;
        if done.actual_len != data.len() {
            return Err(Fault::unexpected("incomplete m1n1 bulk write"));
        }
        Ok(())
    }
}
