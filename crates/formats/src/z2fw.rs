//! Touch controller firmware for Linux's apple_z2 and apple_hidspi_ts
//! drivers, converted from Apple's multitouch data (mtprops plists and MTFW
//! images). The output holds no per-unit data: calibration records tell the
//! driver which device tree property to send where, and m1n1 copies those
//! properties from the ADT at boot.
//!
//! The command orders, packets and configuration words follow Corellium's
//! Project Sandcastle and Paul Praschl's (Pauli1Go) GPL-2.0-or-later
//! makez2fw, as in orchard's Python implementation.
use crate::osserialize::{self, Value};
use crate::{Error, Result};

const MAGIC: u32 = 0x5746_325a;
const INIT: u32 = 0;
const SEND: u32 = 1;
const WAIT: u32 = 3;
const CONFIG: u32 = 4;
const RAW_XFER: u32 = 5;
const CALIBRATION: u32 = 6;

const PACKET_PREFIX: &[u8] = b"\x18\xe1\x30\x01";
const HBPP_INIT: &[u8] = b"\x1a\xa1\x18\xe1";
const REQUEST_CALIBRATION: &[u8] = b"\x1f\x01";
const DEFAULT_WAIT_MS: u32 = 1000;

/// apple_z2 calibration providers, by ADT property name.
const PROVIDERS: &[&str] = &[
    "multi-touch-calibration",
    "orb-gap-cal",
    "orb-force-cal",
    "shape-dynamic-accel-cal",
    "multitouch-to-display-offset",
    "firefly-calibration",
    "pressure-effect-correction",
    "firefly-p100-calibration",
    "ext-multi-touch-calibration",
    "prox-calibration",
];

type Record = (u32, Vec<u8>);

/// A profile from an mtprops plist, and its firmware config where it has one.
pub struct Profile {
    profile: Value,
}

impl Profile {
    pub fn from_mtprops(mtprops: &[u8], name: &str) -> Result<Profile> {
        let root = plist::Value::from_reader(std::io::Cursor::new(mtprops))
            .map_err(|_| Error("mtprops is not a plist"))?;
        let profile = from_plist(root)?
            .get(name)
            .cloned()
            .ok_or(Error("touch profile not found"))?;
        if !matches!(profile, Value::Dict(_)) {
            return Err(Error("touch profile is not a dictionary"));
        }
        Ok(Profile { profile })
    }

    fn config(&self) -> Result<Value> {
        match self.profile.get("Firmware Config") {
            Some(Value::Data(d)) => osserialize::xml(d),
            Some(Value::String(s)) => osserialize::xml(s.as_bytes()),
            _ => Err(Error("touch profile has no firmware config")),
        }
    }

    fn packet_type(&self) -> Option<&str> {
        self.profile
            .get("PreconstructedBootloadPacketType")
            .and_then(Value::as_str)
    }

    fn firmware(&self) -> Result<&[u8]> {
        self.profile
            .get("Constructed Firmware")
            .and_then(Value::as_data)
            .ok_or(Error("touch profile has no constructed firmware"))
    }

    /// "Constructed Firmware" as a list of two or more packets.
    fn firmware_list(&self) -> Result<Vec<&[u8]>> {
        let packets: Option<Vec<&[u8]>> = self
            .profile
            .get("Constructed Firmware")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(Value::as_data).collect())
            .unwrap_or(None);
        packets
            .filter(|packets| packets.len() >= 2)
            .ok_or(Error("touch profile has no list of constructed firmware packets"))
    }
}

/// Whether `packet` is one complete, prefixed Z2 boot packet that loads at
/// address 0: its length, header checksum and payload checksum hold.
fn valid_z2_packet(packet: &[u8]) -> bool {
    packet.len() >= 20
        && packet.starts_with(PACKET_PREFIX)
        && usize::from(be16(&packet[4..6])) * 4 + 16 == packet.len()
        && from_xe32(&packet[6..10]) == 0
        && sum(&packet[4..10]) == u32::from(be16(&packet[10..12]))
        && sum(&packet[12..packet.len() - 4]) == from_xe32(&packet[packet.len() - 4..])
}

/// `z2fw-from-mtprops@1`: one preconstructed HBPP packet with a config.
pub fn from_packet(
    p: &Profile,
    packet: &str,
    load_address: u64,
    hbpp_version: u64,
) -> Result<Vec<u8>> {
    let config = p.config()?;
    if p.packet_type() != Some(packet)
        || config.get("Protocol").and_then(Value::as_str) != Some("Z2")
    {
        return Err(Error("unexpected touch packet type or protocol"));
    }
    let payload = p.firmware()?;
    if decode_blob(payload, 0x104)?.0 != load_address {
        return Err(Error("unexpected touch firmware load address"));
    }
    let calibration = list_or_empty(&config, "Calibration Sequence")?;
    let por = match config.get("Power Off On Reset") {
        Some(Value::Bool(b)) => *b,
        _ => return Err(Error("Power Off On Reset must be a boolean")),
    };
    let spi = field(&config, "SPI Config")?;
    let timeout = field(&config, "Boot Timeout")?;
    let words = [
        ((1 << 9) - 1) | 1 << 11,
        uint(field(&config, "Min DMA Transfer Size")?, u32::MAX)?,
        uint(
            field(field(&config, "Z2 Config")?, "Inter Packet Delay")?,
            u32::MAX,
        )?,
        uint(field(spi, "CS Delay")?, u32::MAX)?,
        uint(field(spi, "Clock Phase")?, 1)?,
        uint(field(spi, "Clock Polarity")?, 1)?,
        uint(field(spi, "Word Delay")?, u32::MAX)?,
        uint(field(spi, "Clock Period Ms")?, u32::MAX)?,
        u32::from(por),
        uint(field(timeout, "Normal Boot Ms")?, 30_000)?,
        uint(&Value::Integer(hbpp_version), 0xffff)?,
    ];
    let first_boot = uint(field(timeout, "First Boot Ms")?, 30_000)?;
    if first_boot == 0 {
        return Err(Error("first boot timeout must be nonzero"));
    }
    let mut records = vec![
        (CONFIG, le32s(&words)),
        (INIT, HBPP_INIT.to_vec()),
        (SEND, payload.to_vec()),
    ];
    let mut providers = Vec::new();
    for item in calibration {
        records.push(calibration_record(
            item.get("Provider").and_then(Value::as_str).unwrap_or(""),
            field(item, "Address")?,
            field(item, "Max Size")?,
            &mut providers,
        )?);
    }
    for item in list(&config, "Boot Sequence")? {
        records.push((SEND, boot_register(item)?));
    }
    records.push((WAIT, le32s(&[first_boot])));
    Ok(container(&records))
}

/// `z2fw-from-z2-packet@1`: one complete Z2 boot packet, wrapped unchanged
/// after the unit's calibration.
pub fn from_z2_packet(
    p: &Profile,
    calibration_address: u64,
    calibration_size: u64,
) -> Result<Vec<u8>> {
    if p.packet_type() != Some("Z2") {
        return Err(Error("touch profile has no preconstructed Z2 packet"));
    }
    let packet = p.firmware()?;
    if !valid_z2_packet(packet) {
        return Err(Error("bad Z2 boot packet"));
    }
    // SPI mode 0 (CPHA and CPOL bits in the valid mask); the driver fills the rest.
    let config = le32s(&[1 << 3 | 1 << 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let calibration = calibration_record(
        "multi-touch-calibration",
        &Value::Integer(calibration_address),
        &Value::Integer(calibration_size),
        &mut Vec::new(),
    )?;
    Ok(container(&[
        (CONFIG, config),
        calibration,
        (SEND, packet.to_vec()),
    ]))
}

/// Where an N1 controller's unit data goes: the calibration and the
/// proximity calibration, each an address and a size in the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct N1 {
    pub calibration_address: u64,
    pub calibration_size: u64,
    pub prox_address: u64,
    pub prox_size: u64,
}

/// `z2fw-from-z2-packet@2`: the N1 controller's one legacy Z2 packet, with
/// a zeroed proximity initialization and the unit's calibration before it.
/// The native driver owns reset, readiness, register calibration and
/// execute: the file holds no INIT, register-write or execute command.
pub fn from_z2_packet_n1(p: &Profile, board: &N1) -> Result<Vec<u8>> {
    let packet = n1_profile(p)?.firmware()?;
    if !valid_z2_packet(packet) {
        return Err(Error("bad Z2 boot packet"));
    }
    n1_container(&[packet], board)
}

/// `z2fw-from-z2-packet@3`: the same for a profile whose constructed
/// firmware is a list of packets (iPhone 6s Plus). The first loads at 0, the
/// others at their own addresses; all are sent in order.
pub fn from_z2_packets_n1(p: &Profile, board: &N1) -> Result<Vec<u8>> {
    let packets = n1_profile(p)?.firmware_list()?;
    if !valid_z2_packet(packets[0]) {
        return Err(Error("bad Z2 boot packet"));
    }
    n1_container(&packets, board)
}

/// An N1 profile: preconstructed Z2 packets and no firmware config, which
/// this layout would not apply.
fn n1_profile(p: &Profile) -> Result<&Profile> {
    if p.profile.get("Firmware Config").is_some() {
        return Err(Error("N1 packet with an unsupported firmware config"));
    }
    if p.packet_type() != Some("Z2") {
        return Err(Error("touch profile has no preconstructed Z2 packet"));
    }
    Ok(p)
}

/// CONFIG (HBPP 0x103), the zeroed proximity initialization, the unit's
/// calibration, then each firmware packet as it is. Unlike HBPP 0x104, a
/// packet's word count includes every payload word.
fn n1_container(packets: &[&[u8]], board: &N1) -> Result<Vec<u8>> {
    const HBPP: u32 = 0x103;
    let loads: Vec<(u64, Vec<u8>)> = packets
        .iter()
        .map(|packet| decode_blob(packet, HBPP))
        .collect::<Result<_>>()?;
    // Both ranges are checked together, for overlap and address overflow.
    let mut providers = Vec::new();
    calibration_record(
        "prox-calibration",
        &Value::Integer(board.prox_address),
        &Value::Integer(board.prox_size),
        &mut providers,
    )?;
    let calibration = calibration_record(
        "multi-touch-calibration",
        &Value::Integer(board.calibration_address),
        &Value::Integer(board.calibration_size),
        &mut providers,
    )?;
    let (prox_address, prox_size) = (providers[0].1, providers[0].2);
    if providers.iter().any(|&(_, address, maximum)| address % 4 != 0 || maximum % 4 != 0) {
        return Err(Error("N1 calibration ranges must be word aligned"));
    }
    let overlap = |a: u64, a_len: u64, b: u64, b_len: u64| a < b + b_len && b < a + a_len;
    for (address, firmware) in &loads {
        let held = providers
            .iter()
            .any(|&(_, a, m)| overlap(a, m.div_ceil(4) * 4, *address, firmware.len() as u64));
        if held {
            return Err(Error("N1 calibration overlaps the firmware load range"));
        }
    }
    for (i, (address, firmware)) in loads.iter().enumerate() {
        let other = loads[i + 1..]
            .iter()
            .any(|(b, second)| overlap(*address, firmware.len() as u64, *b, second.len() as u64));
        if other {
            return Err(Error("N1 firmware packets overlap"));
        }
    }
    // The legacy length field is 16 bits, in words: never wrapped.
    let words = u16::try_from(prox_size / 4)
        .map_err(|_| Error("N1 proximity initialization is too large"))?;
    let mut header = words.to_be_bytes().to_vec();
    header.extend(xe32(prox_address as u32));
    let mut prox = PACKET_PREFIX.to_vec();
    prox.extend(&header);
    prox.extend((sum(&header) as u16).to_be_bytes());
    prox.extend(vec![0u8; prox_size as usize + 4]);
    if decode_blob(&prox, HBPP)? != (prox_address, vec![0u8; prox_size as usize]) {
        return Err(Error("N1 proximity initialization does not read back"));
    }
    // SPI mode 0 and the HBPP version (bits in the valid mask, then the value).
    let mut config = [0u32; 11];
    config[0] = 1 << 3 | 1 << 4 | 1 << 11;
    config[10] = HBPP;
    let mut records = vec![(CONFIG, le32s(&config)), (SEND, prox), calibration];
    records.extend(packets.iter().map(|packet| (SEND, packet.to_vec())));
    Ok(container(&records))
}

/// `z2fw-from-mtprops@2`: an array of preconstructed packets (makez2fw's
/// Gen2 path with dynamic calibration).
pub fn from_packets(p: &Profile, packet: &str) -> Result<Vec<u8>> {
    if p.packet_type() != Some(packet) {
        return Err(Error("unexpected touch packet type"));
    }
    let packets = p
        .profile
        .get("Constructed Firmware")
        .and_then(Value::as_array)
        .filter(|packets| !packets.is_empty())
        .ok_or(Error("expected a packet array"))?;
    let config = p.config()?;
    let nested = match config.get("Config") {
        Some(c @ Value::Dict(_)) => c,
        _ => &config,
    };
    let hbpp = hbpp_config(nested, false)?;
    let mut records = vec![(INIT, HBPP_INIT.to_vec())];
    for item in packets {
        match item.as_data() {
            Some(d) if !d.is_empty() => records.push((SEND, d.to_vec())),
            _ => return Err(Error("expected nonempty packets")),
        }
    }
    if let Some(words) = hbpp.words {
        records.push((CONFIG, words));
    }
    let mut providers = Vec::new();
    for item in list(&config, "Calibration Sequence")? {
        records.push(calibration_record(
            field(item, "Provider")?.as_str().unwrap_or(""),
            field(item, "Address")?,
            field(item, "Max Size")?,
            &mut providers,
        )?);
    }
    for item in list(&config, "Boot Sequence")? {
        if item.get("Action").and_then(Value::as_str) == Some("RequestCalibration")
            && dict_len(item) == 1
        {
            records.push((SEND, REQUEST_CALIBRATION.to_vec()));
        } else {
            records.push((SEND, boot_register(item)?));
        }
    }
    records.push((WAIT, le32s(&[hbpp.wait_ms])));
    Ok(container(&records))
}

/// Board values `from_gen1` needs, from Apple's matching kernel personality.
pub struct Gen1 {
    pub touch_maximum: u64,
    pub prox_maximum: u64,
    pub fll_mval: u64,
    pub ref_clk_div: u64,
    pub clk32_clock_enable: u64,
}

/// `z2fw-from-mtprops@3`: a legacy Gen1 packet (Project Sandcastle's
/// command order), with calibration records in place of SysCfg loads.
pub fn from_gen1(p: &Profile, packet: &str, board: &Gen1) -> Result<Vec<u8>> {
    if packet != "Z2"
        || p.packet_type() != Some(packet)
        || p.profile.get("Firmware Config").is_some()
    {
        return Err(Error("unexpected Gen1 touch profile"));
    }
    let payload = p.firmware()?;
    if decode_blob(payload, 0x103)?.0 != 0 {
        return Err(Error("unexpected Gen1 firmware load address"));
    }
    let mut providers = Vec::new();
    let mut hbpp = HBPP_INIT.to_vec();
    hbpp.extend(b"\x18\xe1".repeat(6));
    let mut records = vec![
        (INIT, b"\x19\xc1".to_vec()),
        (INIT, hbpp.clone()),
        (INIT, hbpp.clone()),
        (INIT, hbpp),
    ];
    let n = |v: u64| Value::Integer(v);
    records.push(calibration_record(
        "prox-calibration",
        &n(0x1000_9600),
        &n(board.prox_maximum),
        &mut providers,
    )?);
    records.push(calibration_record(
        "multi-touch-calibration",
        &n(0x1000_9000),
        &n(board.touch_maximum),
        &mut providers,
    )?);
    records.push((SEND, payload.to_vec()));
    for (address, value) in [
        (0x1000_3060, board.fll_mval),
        (0x1000_305c, board.ref_clk_div),
        (0x1000_3058, 6),
        (0x1000_3000, 3),
        (0x1000_3518, board.clk32_clock_enable),
    ] {
        records.push((SEND, register_packet(address, 0xffff_ffff, value)?));
    }
    records.push((SEND, REQUEST_CALIBRATION.to_vec()));
    records.push((
        INIT,
        vec![
            0x1d, 0x53, 0x34, 0x00, 0x10, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x45,
        ],
    ));
    Ok(container(&records))
}

/// `z2fw-from-mtfw@1`: an MTFW personality listing HBPP load items
/// (makez2fw's item path). Each calibration property loads Apple's default,
/// then a calibration record, so a unit without the property keeps it.
pub fn from_mtfw(mtfw: &[u8], profile: &str, otp_preflight: bool) -> Result<Vec<u8>> {
    let items = personality(mtfw, profile)?;
    let config = one_config(&items, "Z2")?;
    let hbpp = hbpp_config(config, true)?;
    let mut records: Vec<Record> = hbpp.words.map(|w| (CONFIG, w)).into_iter().collect();
    records.push((INIT, HBPP_INIT.to_vec()));
    if let (true, Some(otp)) = (otp_preflight, hbpp.otp) {
        records.extend(otp_records(otp)?);
    }
    let mut providers = Vec::new();
    for item in &items {
        let kind = item.get("Type").and_then(Value::as_str).unwrap_or("");
        if kind == "Config" {
            continue;
        }
        let address = uint(
            item.get("Address")
                .ok_or(Error("HBPP item has no address"))?,
            u32::MAX,
        )?;
        match kind {
            "ReadModifyWrite" => records.push((SEND, rmw(item, address)?)),
            "Binary" => {
                if truthy(item.get("SysConfigKey")) {
                    return Err(Error("HBPP binary item with per-unit data"));
                }
                records.push((SEND, encode_blob(address, data(item, "Payload")?)?));
            }
            "Property" => {
                // Apple's default for a unit without the property, where
                // there is one; anything else than data there is refused.
                if truthy(item.get("Payload")) {
                    records.push((SEND, encode_blob(address, data(item, "Payload")?)?));
                }
                records.push(calibration_record(
                    item.get("Property").and_then(Value::as_str).unwrap_or(""),
                    &Value::Integer(address.into()),
                    item.get("MaxSize")
                        .ok_or(Error("calibration item has no size"))?,
                    &mut providers,
                )?);
            }
            _ => return Err(Error("unsupported HBPP item type")),
        }
    }
    records.push((WAIT, le32s(&[hbpp.wait_ms])));
    Ok(container(&records))
}

/// `hidspi-from-mtfw@1`: an MTFW personality of a HIDSPI controller as the
/// op list apple_hidspi_ts replays: "D221OPS2", then little-endian ops
/// T (label), S (HBPP packet with expected ack), P (poll a register),
/// R (read and log a register) and C (calibration, with Apple's default).
pub fn hidspi_from_mtfw(mtfw: &[u8], profile: &str) -> Result<Vec<u8>> {
    const ACK_BLOB: u16 = 0x4bc1;
    const ACK_WRITE: u16 = 0x4ad1;
    let items = personality(mtfw, profile)?;
    one_config(&items, "HIDSPI")?;
    let read_on_error = items.iter().find(|i| {
        i.get("Type").and_then(Value::as_str) == Some("Metadata")
            && i.get("Tag").and_then(Value::as_str) == Some("ReadOnError")
    });
    let mut out = b"D221OPS2".to_vec();
    let label = |out: &mut Vec<u8>, text: String| {
        out.push(b'T');
        out.extend((text.len() as u32).to_le_bytes());
        out.extend(text.as_bytes());
    };
    let send = |out: &mut Vec<u8>, packet: Vec<u8>, ack: u16| {
        out.push(b'S');
        out.extend(ack.to_le_bytes());
        out.extend((packet.len() as u32).to_le_bytes());
        out.extend(packet);
    };
    let read = |out: &mut Vec<u8>, item: &Value| -> Result<()> {
        out.push(b'R');
        out.extend(
            uint(
                item.get("Address")
                    .ok_or(Error("ReadOnError has no address"))?,
                u32::MAX,
            )?
            .to_le_bytes(),
        );
        Ok(())
    };
    if let Some(item) = read_on_error {
        read(&mut out, item)?;
    }
    for (index, item) in items.iter().enumerate() {
        let kind = item.get("Type").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "Metadata" | "Config") {
            continue;
        }
        let address = uint(
            item.get("Address")
                .ok_or(Error("HBPP item has no address"))?,
            u32::MAX,
        )?;
        let name = [item.get("Description"), item.get("Tag")]
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|s| !s.is_empty())
            .unwrap_or("");
        match kind {
            "ReadModifyWrite" => {
                let mask = item
                    .get("Mask")
                    .map(|m| uint(m, u32::MAX))
                    .transpose()?
                    .unwrap_or(u32::MAX);
                let value = item
                    .get("Value")
                    .and_then(Value::as_u64)
                    .ok_or(Error("ReadModifyWrite without an integer value"))?;
                label(
                    &mut out,
                    format!("{index} write {address:#x} mask {mask:#x} = {value:#x} {name}")
                        .trim_end()
                        .to_owned(),
                );
                let skip = match item.get("SkipCheck") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(Error("SkipCheck must be a boolean")),
                };
                send(
                    &mut out,
                    rmw(item, address)?,
                    if skip { 0 } else { ACK_WRITE },
                );
            }
            "Binary" => {
                if truthy(item.get("SysConfigKey")) {
                    return Err(Error("HBPP binary item with per-unit data"));
                }
                let payload = data(item, "Payload")?;
                label(
                    &mut out,
                    format!("{index} load {address:#x} {} bytes {name}", payload.len())
                        .trim_end()
                        .to_owned(),
                );
                send(&mut out, encode_blob(address, payload)?, ACK_BLOB);
            }
            "Property" => {
                let prop = item
                    .get("Property")
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                    .ok_or(Error("calibration item has no property"))?;
                let maximum = uint(
                    item.get("MaxSize")
                        .ok_or(Error("calibration item has no size"))?,
                    u32::MAX,
                )?;
                if maximum == 0 {
                    return Err(Error("empty calibration range"));
                }
                // m1n1 copies the touch calibration under this name; others keep apple,<ADT name>.
                let dt_name = if prop == "multi-touch-calibration" {
                    "apple,z2-cal-blob".to_owned()
                } else {
                    format!("apple,{prop}")
                };
                label(
                    &mut out,
                    format!("{index} calibration {address:#x} up to {maximum} bytes {dt_name}"),
                );
                let default = match item.get("Payload") {
                    None => &[][..],
                    Some(v) => v
                        .as_data()
                        .ok_or(Error("calibration default is not data"))?,
                };
                out.push(b'C');
                for n in [address, maximum, dt_name.len() as u32] {
                    out.extend(n.to_le_bytes());
                }
                out.extend(dt_name.as_bytes());
                out.extend((default.len() as u32).to_le_bytes());
                out.extend(default);
            }
            "Poll" => {
                label(
                    &mut out,
                    format!("{index} poll {address:#x} {name}")
                        .trim_end()
                        .to_owned(),
                );
                out.push(b'P');
                let get = |k: &str| item.get(k).ok_or(Error("poll item is incomplete"));
                for n in [
                    address,
                    uint(get("Mask")?, u32::MAX)?,
                    uint(get("Value")?, u32::MAX)?,
                    uint(get("TimeoutMs")?, 60_000)?,
                ] {
                    out.extend(n.to_le_bytes());
                }
            }
            _ => return Err(Error("unsupported HBPP item type")),
        }
    }
    if let Some(item) = read_on_error {
        read(&mut out, item)?;
    }
    Ok(out)
}

struct Hbpp {
    words: Option<Vec<u8>>,
    wait_ms: u32,
    /// OTP address, index and size for the preflight read.
    otp: Option<(u64, u64, u64)>,
}

/// makez2fw's configuration words: each present field sets its bit in the
/// valid mask (word 0).
fn hbpp_config(config: &Value, overrides: bool) -> Result<Hbpp> {
    let section = |key: &str| -> Result<Option<&Value>> {
        match config.get(key) {
            None => Ok(None),
            Some(v @ Value::Dict(_)) => Ok(Some(v)),
            Some(_) => Err(Error("unexpected touch config section")),
        }
    };
    let (z2, spi, timeout) = (
        section("Z2 Config")?,
        section("SPI Config")?,
        section("Boot Timeout")?,
    );
    let mut values = [0u32; 11];
    let mut valid = 0u32;
    let fields = [
        (Some(config), "Min DMA Transfer Size", 0, 1),
        (z2, "Inter Packet Delay", 1, 2),
        (spi, "CS Delay", 2, 3),
        (spi, "Clock Phase", 3, 4),
        (spi, "Clock Polarity", 4, 5),
        (spi, "Word Delay", 5, 6),
        (spi, "Clock Period Ms", 6, 7),
        (timeout, "Normal Boot Ms", 8, 9),
    ];
    for (container, key, bit, index) in fields {
        if let Some(v) = container.and_then(|c| c.get(key)) {
            values[index] = uint(v, u32::MAX)?;
            valid |= 1 << bit;
        }
    }
    match config.get("Power Off On Reset") {
        None => {}
        Some(Value::Bool(b)) => {
            values[8] = u32::from(*b);
            valid |= 1 << 7;
        }
        Some(_) => return Err(Error("Power Off On Reset must be a boolean")),
    }
    let mut otp = None;
    if overrides {
        let dto = section("Device Tree Overrides")?;
        let get = |key: &str, min: usize| {
            dto.and_then(|d| d.get(key))
                .and_then(Value::as_data)
                .filter(|d| d.len() >= min)
        };
        let le = |d: &[u8], at: usize| u32::from_le_bytes(d[at..at + 4].try_into().unwrap());
        let (otp_address, hbpp_version, otp_sn) = (
            get("otp-address", 4),
            get("hbpp-version", 4),
            get("otp-sn", 8),
        );
        for (present, bit) in [
            (otp_address.is_some(), 9),
            (get("chip-id-address", 4).is_some(), 10),
            (hbpp_version.is_some(), 11),
            (otp_sn.is_some(), 12),
            (get("reset-sequence", 1).is_some(), 13),
            (get("power-sequence", 1).is_some(), 14),
        ] {
            if present {
                valid |= 1 << bit;
            }
        }
        if let Some(v) = hbpp_version {
            values[10] = le(v, 0);
        }
        if let (Some(a), Some(sn)) = (otp_address, otp_sn) {
            otp = Some((le(a, 0).into(), le(sn, 0).into(), le(sn, 4).into()));
        }
    }
    values[0] = valid;
    let wait_ms = if valid & 1 << 8 != 0 && values[9] != 0 {
        values[9]
    } else {
        DEFAULT_WAIT_MS
    };
    Ok(Hbpp {
        words: (valid != 0).then(|| le32s(&values)),
        wait_ms,
        otp,
    })
}

/// makez2fw's OTP read sequence before the first load.
fn otp_records((address, index, size): (u64, u64, u64)) -> Result<Vec<Record>> {
    if address == 0 || size == 0 || size % 4 != 0 || size > 256 {
        return Err(Error("bad OTP address or selection"));
    }
    let write = |offset: u64, value: u64| -> Result<Record> {
        Ok((SEND, register_packet(address + offset, 0xffff_ffff, value)?))
    };
    let read = |at: u64| -> Result<Vec<Record>> {
        let fields = xe32(u32::try_from(at).map_err(|_| Error("register address out of range"))?);
        let mut request = b"\x1c\x73".to_vec();
        request.extend(fields);
        request.extend((sum(&fields) as u16).to_be_bytes());
        let mut clock = HBPP_INIT.to_vec();
        clock.extend(b"\x18\xe1".repeat(2));
        Ok(vec![
            (RAW_XFER, raw_xfer(&request, 0)),
            (RAW_XFER, raw_xfer(&clock, 8)),
        ])
    };
    let mut records = vec![write(0x00, 2)?, write(0x04, 0)?, write(0x08, 0)?];
    for word in 0..size / 4 {
        records.extend([
            write(0x10, 0xffff_ffff)?,
            write(0x0c, index + word)?,
            write(0x04, 0x00e8_0000)?,
            write(0x04, 0x00e8_0001)?,
        ]);
        records.extend(read(address + 0x20)?);
        records.push(write(0x20, 1)?);
        records.extend(read(address + 0x18)?);
    }
    Ok(records)
}

/// Z2FW command 5: tx length, rx length, flags (rx bit reverse), tx bytes.
fn raw_xfer(tx: &[u8], rx: u32) -> Vec<u8> {
    let mut out = le32s(&[tx.len() as u32, rx, 1]);
    out.extend(tx);
    out
}

/// The items of an MTFW personality.
fn personality(mtfw: &[u8], profile: &str) -> Result<Vec<Value>> {
    let doc = osserialize::xml(mtfw)?;
    let items = doc
        .get(profile)
        .and_then(Value::as_array)
        .ok_or(Error("MTFW personality not found"))?;
    if !items.iter().all(|i| matches!(i, Value::Dict(_))) {
        return Err(Error("MTFW personality item is not a dictionary"));
    }
    Ok(items.to_vec())
}

fn one_config<'a>(items: &'a [Value], protocol: &str) -> Result<&'a Value> {
    let mut configs = items
        .iter()
        .filter(|i| i.get("Type").and_then(Value::as_str) == Some("Config"))
        .map(|i| i.get("Config"));
    match (configs.next(), configs.next()) {
        (Some(Some(c)), None) if c.get("Protocol").and_then(Value::as_str) == Some(protocol) => {
            Ok(c)
        }
        _ => Err(Error(
            "MTFW personality does not have exactly one config for this protocol",
        )),
    }
}

/// A calibration record: apple_z2 sends the provider's device tree property
/// to `address`. `providers` holds earlier records, to refuse repeats and
/// overlaps.
fn calibration_record(
    name: &str,
    address: &Value,
    maximum: &Value,
    providers: &mut Vec<(String, u64, u64)>,
) -> Result<Record> {
    let id = PROVIDERS
        .iter()
        .position(|p| *p == name)
        .ok_or(Error("no apple_z2 calibration provider with this name"))?;
    if providers.iter().any(|(p, _, _)| p == name) {
        return Err(Error("repeated calibration provider"));
    }
    let maximum = u64::from(uint(maximum, 0x40000)?);
    let address = u64::from(uint(address, u32::MAX)?);
    if maximum == 0 || address == 0 {
        return Err(Error("calibration range is empty or at address 0"));
    }
    let end = address + maximum.div_ceil(4) * 4;
    if end > 1 << 32
        || providers
            .iter()
            .any(|&(_, a, m)| address < a + m.div_ceil(4) * 4 && a < end)
    {
        return Err(Error("calibration ranges overlap or overflow"));
    }
    providers.push((name.to_owned(), address, maximum));
    Ok((
        CALIBRATION,
        le32s(&[address as u32, maximum as u32, id as u32]),
    ))
}

/// An HBPP blob packet carrying a payload, with the word-count-minus-one
/// length that `decode_blob` reads back.
fn encode_blob(address: u32, payload: &[u8]) -> Result<Vec<u8>> {
    let mut padded = payload.to_vec();
    padded.resize(payload.len().div_ceil(4) * 4, 0);
    if payload.is_empty()
        || padded.len() > 0x40000
        || u64::from(address) + padded.len() as u64 > 1 << 32
    {
        return Err(Error("HBPP payload is empty or too large"));
    }
    let mut header = ((padded.len() / 4 - 1) as u16).to_be_bytes().to_vec();
    header.extend(xe32(address));
    let mut out = PACKET_PREFIX.to_vec();
    out.extend(&header);
    out.extend((sum(&header) as u16).to_be_bytes());
    out.extend(swapped(&padded));
    out.extend(xe32(sum(payload)));
    Ok(out)
}

/// The load address and payload of an HBPP blob packet.
fn decode_blob(packet: &[u8], hbpp_version: u32) -> Result<(u64, Vec<u8>)> {
    if packet.len() < 20 || !packet.starts_with(PACKET_PREFIX) {
        return Err(Error("not an HBPP blob packet"));
    }
    let size = (usize::from(be16(&packet[4..6])) + usize::from(hbpp_version >= 0x104)) * 4;
    if packet.len() != 16 + size || sum(&packet[4..10]) != u32::from(be16(&packet[10..12])) {
        return Err(Error("HBPP packet length or header checksum mismatch"));
    }
    let payload = swapped(&packet[12..packet.len() - 4]);
    if sum(&payload) != from_xe32(&packet[packet.len() - 4..]) {
        return Err(Error("HBPP payload checksum mismatch"));
    }
    let address = u64::from(from_xe32(&packet[6..10]));
    if address + size as u64 > 1 << 32 {
        return Err(Error("HBPP address range overflows"));
    }
    Ok((address, payload))
}

/// A masked register write packet.
fn register_packet(address: u64, mask: u64, value: u64) -> Result<Vec<u8>> {
    let mut fields = Vec::new();
    for n in [address, mask, value] {
        fields.extend(xe32(
            u32::try_from(n).map_err(|_| Error("register value out of range"))?,
        ));
    }
    let mut out = b"\x1e\x33".to_vec();
    out.extend(&fields);
    out.extend((sum(&fields) as u16).to_be_bytes());
    Ok(out)
}

fn boot_register(item: &Value) -> Result<Vec<u8>> {
    let Value::Dict(entries) = item else {
        return Err(Error("unsupported boot operation"));
    };
    let allowed = entries
        .iter()
        .all(|(k, _)| matches!(k.as_str(), "Address" | "Value" | "Mask"));
    if !allowed || item.get("Address").is_none() || item.get("Value").is_none() {
        return Err(Error("unsupported boot operation"));
    }
    let mask = item
        .get("Mask")
        .map(|m| uint(m, u32::MAX))
        .transpose()?
        .unwrap_or(u32::MAX);
    register_packet(
        uint(field(item, "Address")?, u32::MAX)?.into(),
        mask.into(),
        uint(field(item, "Value")?, u32::MAX)?.into(),
    )
}

fn rmw(item: &Value, address: u32) -> Result<Vec<u8>> {
    let value = uint(
        item.get("Value")
            .ok_or(Error("ReadModifyWrite without a value"))?,
        u32::MAX,
    )?;
    let mask = item
        .get("Mask")
        .map(|m| uint(m, u32::MAX))
        .transpose()?
        .unwrap_or(u32::MAX);
    register_packet(address.into(), mask.into(), value.into())
}

fn container(records: &[Record]) -> Vec<u8> {
    let mut out = le32s(&[MAGIC, 1]);
    for (code, payload) in records {
        out.extend(le32s(&[*code, payload.len() as u32]));
        out.extend(payload);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    out
}

/// An integer up to `maximum`; decimal strings count, booleans do not.
fn uint(value: &Value, maximum: u32) -> Result<u32> {
    let n = match value {
        Value::Integer(n) => Some(*n),
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().ok()
        }
        _ => None,
    };
    n.filter(|&n| n <= u64::from(maximum))
        .map(|n| n as u32)
        .ok_or(Error("touch config integer out of range"))
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a Value> {
    v.get(key).ok_or(Error("touch config is missing a field"))
}

fn list<'a>(v: &'a Value, key: &str) -> Result<&'a [Value]> {
    field(v, key)?
        .as_array()
        .ok_or(Error("touch config field is not a list"))
}

fn list_or_empty<'a>(v: &'a Value, key: &str) -> Result<&'a [Value]> {
    v.get(key).map_or(Ok(&[][..]), |l| {
        l.as_array()
            .ok_or(Error("touch config field is not a list"))
    })
}

fn data<'a>(v: &'a Value, key: &str) -> Result<&'a [u8]> {
    v.get(key)
        .and_then(Value::as_data)
        .ok_or(Error("HBPP item has no payload"))
}

fn dict_len(v: &Value) -> usize {
    match v {
        Value::Dict(e) => e.len(),
        _ => 0,
    }
}

/// Python-style truthiness, as makez2fw's data uses it for optional keys.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Integer(n)) => *n != 0,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Data(d)) => !d.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
    }
}

fn from_plist(v: plist::Value) -> Result<Value> {
    Ok(match v {
        plist::Value::Dictionary(d) => Value::Dict(
            d.into_iter()
                .map(|(k, v)| Ok((k, from_plist(v)?)))
                .collect::<Result<_>>()?,
        ),
        plist::Value::Array(a) => {
            Value::Array(a.into_iter().map(from_plist).collect::<Result<_>>()?)
        }
        plist::Value::String(s) => Value::String(s),
        plist::Value::Data(d) => Value::Data(d),
        plist::Value::Boolean(b) => Value::Bool(b),
        plist::Value::Integer(i) => Value::Integer(
            i.as_unsigned()
                .ok_or(Error("negative integer in mtprops"))?,
        ),
        _ => return Err(Error("unsupported value in mtprops")),
    })
}

fn le32s(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn be16(d: &[u8]) -> u16 {
    u16::from_be_bytes([d[0], d[1]])
}

/// Two big-endian halves, low half first.
fn xe32(v: u32) -> [u8; 4] {
    let [a, b] = (v as u16).to_be_bytes();
    let [c, d] = ((v >> 16) as u16).to_be_bytes();
    [a, b, c, d]
}

fn from_xe32(d: &[u8]) -> u32 {
    u32::from(be16(&d[0..2])) | u32::from(be16(&d[2..4])) << 16
}

fn swapped(d: &[u8]) -> Vec<u8> {
    d.chunks(2)
        .flat_map(|p| [p.get(1).copied().unwrap_or(0), p[0]])
        .collect()
}

fn sum(d: &[u8]) -> u32 {
    d.iter().fold(0u32, |s, &b| s.wrapping_add(b.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_round_trip() {
        let packet = encode_blob(0x0010_0000, b"firmware").unwrap();
        assert_eq!(
            decode_blob(&packet, 0x104).unwrap(),
            (0x0010_0000, b"firmware".to_vec())
        );
        let mut damaged = packet.clone();
        damaged[14] ^= 1;
        assert!(decode_blob(&damaged, 0x104).is_err());
    }

    #[test]
    fn calibration_ranges_must_not_overlap() {
        let n = Value::Integer;
        let mut providers = Vec::new();
        calibration_record("orb-gap-cal", &n(0x1000), &n(16), &mut providers).unwrap();
        assert!(calibration_record("orb-gap-cal", &n(0x2000), &n(16), &mut providers).is_err());
        assert!(calibration_record("prox-calibration", &n(0x100c), &n(4), &mut providers).is_err());
        assert!(calibration_record("unknown", &n(0x3000), &n(4), &mut providers).is_err());
        assert!(calibration_record("prox-calibration", &n(0x1010), &n(4), &mut providers).is_ok());
    }

    #[test]
    fn container_pads_records() {
        let c = container(&[(SEND, vec![1, 2, 3])]);
        assert_eq!(c, [le32s(&[MAGIC, 1, SEND, 3]), vec![1, 2, 3, 0]].concat());
    }
}
