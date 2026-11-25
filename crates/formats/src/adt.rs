//! Apple's device tree (ADT), as iBoot hands it to what it starts: nodes of
//! properties (a 32-byte name, a length, the value padded to four bytes) and
//! child nodes, depth first.
use crate::{Error, Result};

/// The largest tree taken.
pub const LIMIT: usize = 16 << 20;

/// Which unit a tree is of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// `/chosen`'s `unique-chip-id`.
    pub ecid: u64,
    /// The root's `model`, such as `iPad7,5`.
    pub model: Option<String>,
    /// The root's `serial-number`.
    pub serial_number: Option<String>,
}

struct Walk<'a> {
    data: &'a [u8],
    position: usize,
    budget: u32,
    ecids: Vec<u64>,
    model: Option<String>,
    serial_number: Option<String>,
}

fn text(value: &[u8]) -> Option<String> {
    let end = value.iter().position(|&b| b == 0).unwrap_or(value.len());
    let text = std::str::from_utf8(&value[..end]).ok()?;
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic())).then(|| text.to_owned())
}

impl<'a> Walk<'a> {
    fn word(&self, at: usize) -> Result<usize> {
        let bytes = self.data.get(at..at + 4).ok_or(Error("ADT ends inside a node"))?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")) as usize)
    }

    /// Reads one node and its children. `parent`: 0 above the root, 1 for
    /// the root's children, 2 below. Returns the node's name.
    fn node(&mut self, depth: u32, parent: u8) -> Result<&'a [u8]> {
        self.budget = self.budget.checked_sub(1).ok_or(Error("ADT has too many nodes"))?;
        if depth > 64 {
            return Err(Error("ADT is nested too deeply"));
        }
        let (properties, children) = (self.word(self.position)?, self.word(self.position + 4)?);
        self.position += 8;
        if properties > 4096 || children > 4096 {
            return Err(Error("ADT node count is out of bounds"));
        }
        let mut name: &[u8] = b"";
        let (mut chip, mut model, mut serial) = (None, None, None);
        for _ in 0..properties {
            let label = self.data.get(self.position..self.position + 32).ok_or(Error("ADT ends inside a property"))?;
            let label = &label[..label.iter().position(|&b| b == 0).unwrap_or(32)];
            let size = self.word(self.position + 32)? & 0xff_ffff;
            self.position += 36;
            let padded = (size + 3) & !3;
            let value = self.data.get(self.position..self.position + padded).ok_or(Error("ADT property is larger than the tree"))?;
            let value = &value[..size];
            match label {
                b"name" => name = &value[..value.iter().position(|&b| b == 0).unwrap_or(size)],
                b"unique-chip-id" => chip = Some(value),
                b"model" => model = text(value),
                b"serial-number" => serial = text(value),
                _ => {}
            }
            self.position += padded;
        }
        match parent {
            0 => {
                self.model = model;
                self.serial_number = serial;
            }
            1 if name == b"chosen" => {
                let chip: [u8; 8] = chip.and_then(|c| c.try_into().ok()).ok_or(Error("ADT's chosen node has no device identity"))?;
                self.ecids.push(u64::from_le_bytes(chip));
            }
            _ => {}
        }
        for _ in 0..children {
            self.node(depth + 1, (parent + 1).min(2))?;
        }
        Ok(name)
    }
}

/// Checks that `tree` is one whole device tree and says which unit's.
pub fn unit(tree: &[u8]) -> Result<Unit> {
    if !(8..=LIMIT).contains(&tree.len()) {
        return Err(Error("ADT length is out of bounds"));
    }
    let mut walk = Walk { data: tree, position: 0, budget: 100_000, ecids: Vec::new(), model: None, serial_number: None };
    if walk.node(0, 0)? != b"device-tree" {
        return Err(Error("ADT's root is not device-tree"));
    }
    match walk.ecids.as_slice() {
        [ecid] if *ecid != 0 => Ok(Unit { ecid: *ecid, model: walk.model, serial_number: walk.serial_number }),
        _ => Err(Error("ADT does not name exactly one device")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property(name: &str, value: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 32];
        out[..name.len()].copy_from_slice(name.as_bytes());
        // The top byte holds flags, which are not part of the length.
        out.extend((value.len() as u32 | 0x8000_0000).to_le_bytes());
        out.extend(value);
        out.resize(out.len() + (4 - value.len() % 4) % 4, 0);
        out
    }

    fn node(properties: &[Vec<u8>], children: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (properties.len() as u32).to_le_bytes().to_vec();
        out.extend((children.len() as u32).to_le_bytes());
        out.extend(properties.concat());
        out.extend(children.concat());
        out
    }

    fn tree(chosen: &[Vec<u8>]) -> Vec<u8> {
        let chosen = node(&[vec![property("name", b"chosen\0")], chosen.to_vec()].concat(), &[]);
        // A chosen further down is not the device's.
        let decoy = node(&[property("name", b"chosen\0"), property("unique-chip-id", &[9; 8])], &[]);
        let other = node(&[property("name", b"arm-io\0")], &[decoy]);
        node(
            &[property("name", b"device-tree\0"), property("model", b"iPad7,5\0"), property("serial-number", b"F4KXY12ZAB\0\0\0")],
            &[chosen, other],
        )
    }

    #[test]
    fn a_tree_says_which_unit_it_is_of() {
        let ecid = 0x000a_45d0_28bb_e0a6u64;
        let unit = unit(&tree(&[property("unique-chip-id", &ecid.to_le_bytes())])).unwrap();
        assert_eq!(unit, Unit { ecid, model: Some("iPad7,5".into()), serial_number: Some("F4KXY12ZAB".into()) });
    }

    #[test]
    fn a_tree_that_is_not_whole_or_names_no_device_is_refused() {
        let good = tree(&[property("unique-chip-id", &7u64.to_le_bytes())]);
        for cut in [0, 7, 8, 40, good.len() - 1] {
            assert!(unit(&good[..cut]).is_err(), "cut at {cut}");
        }
        assert!(unit(&tree(&[])).is_err());
        assert!(unit(&tree(&[property("unique-chip-id", &[1, 2, 3, 4])])).is_err());
        assert!(unit(&tree(&[property("unique-chip-id", &[0; 8])])).is_err());
        assert!(unit(&node(&[property("name", b"other\0")], &[])).is_err());
        // Counts that cannot be true end it before anything is read for them.
        let mut wild = good.clone();
        wild[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(unit(&wild).is_err());
    }
}
