//! `BuildManifest.plist`: which files of a firmware belong to which board.
use crate::{Error, Result};
use plist::{Dictionary, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub build: String,
    pub identities: Vec<Identity>,
}

/// One board and restore kind, with its components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub chip: u32,
    pub board: u32,
    /// The board name, such as `n71ap`.
    pub device_class: String,
    /// `Erase` for a full restore, `Update` for an update.
    pub behavior: String,
    /// Component name (`iBSS`, `DeviceTree`, `OS`, …) to its path in the archive.
    pub components: BTreeMap<String, String>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        let value = Value::from_reader(std::io::Cursor::new(bytes))
            .map_err(|_| Error("BuildManifest is not a plist"))?;
        let root = value
            .as_dictionary()
            .ok_or(Error("BuildManifest is not a dictionary"))?;
        let identities = root
            .get("BuildIdentities")
            .and_then(Value::as_array)
            .ok_or(Error("BuildManifest has no build identities"))?
            .iter()
            .map(identity)
            .collect::<Result<_>>()?;
        Ok(Manifest {
            version: string(root, "ProductVersion")?,
            build: string(root, "ProductBuildVersion")?,
            identities,
        })
    }

    /// The identity for a board, preferring a full restore over an update.
    pub fn identity(&self, device_class: &str) -> Result<&Identity> {
        let mut matching = self
            .identities
            .iter()
            .filter(|i| i.device_class.eq_ignore_ascii_case(device_class));
        let first = matching
            .next()
            .ok_or(Error("firmware has no build identity for this board"))?;
        Ok(std::iter::once(first)
            .chain(matching)
            .find(|i| i.behavior == "Erase")
            .unwrap_or(first))
    }

    /// The identity for a board by its chip and board numbers, preferring a
    /// full restore over an update.
    pub fn identity_for(&self, chip: u32, board: u32) -> Result<&Identity> {
        let mut matching = self
            .identities
            .iter()
            .filter(|i| (i.chip, i.board) == (chip, board));
        let first = matching.next().ok_or(Error(
            "firmware has no build identity for this chip and board",
        ))?;
        Ok(std::iter::once(first)
            .chain(matching)
            .find(|i| i.behavior == "Erase")
            .unwrap_or(first))
    }

    /// The board names this firmware covers.
    pub fn device_classes(&self) -> Vec<&str> {
        let mut classes: Vec<_> = self
            .identities
            .iter()
            .map(|i| i.device_class.as_str())
            .collect();
        classes.sort_unstable();
        classes.dedup();
        classes
    }
}

fn identity(value: &Value) -> Result<Identity> {
    let d = value.as_dictionary().ok_or(Error("bad build identity"))?;
    let info = d
        .get("Info")
        .and_then(Value::as_dictionary)
        .ok_or(Error("build identity has no info"))?;
    let components = d
        .get("Manifest")
        .and_then(Value::as_dictionary)
        .ok_or(Error("build identity has no components"))?
        .iter()
        .filter_map(|(name, c)| {
            let path = c
                .as_dictionary()?
                .get("Info")?
                .as_dictionary()?
                .get("Path")?
                .as_string()?;
            Some((name.clone(), path.to_owned()))
        })
        .collect();
    Ok(Identity {
        chip: hex(d, "ApChipID")?,
        board: hex(d, "ApBoardID")?,
        device_class: string(info, "DeviceClass")?,
        behavior: info
            .get("RestoreBehavior")
            .and_then(Value::as_string)
            .unwrap_or_default()
            .to_owned(),
        components,
    })
}

fn string(d: &Dictionary, key: &str) -> Result<String> {
    d.get(key)
        .and_then(Value::as_string)
        .map(str::to_owned)
        .ok_or(Error("BuildManifest is missing a field"))
}

fn hex(d: &Dictionary, key: &str) -> Result<u32> {
    let s = string(d, key)?;
    u32::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|_| Error("BuildManifest has a bad chip or board id"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let identity = |class: &str, board: &str, behavior: &str, os: &str| {
            format!(
                "<dict><key>ApChipID</key><string>0x8010</string><key>ApBoardID</key><string>{board}</string>\
                 <key>Info</key><dict><key>DeviceClass</key><string>{class}</string>\
                 <key>RestoreBehavior</key><string>{behavior}</string></dict>\
                 <key>Manifest</key><dict><key>OS</key><dict><key>Info</key><dict><key>Path</key><string>{os}</string></dict></dict>\
                 <key>iBSS</key><dict><key>Info</key><dict><key>Path</key><string>Firmware/dfu/iBSS.{class}.im4p</string></dict></dict>\
                 </dict></dict>"
            )
        };
        format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
             <key>ProductVersion</key><string>17.7</string><key>ProductBuildVersion</key><string>21H16</string>\
             <key>BuildIdentities</key><array>{}{}{}</array></dict></plist>",
            identity("x1ap", "0x02", "Update", "update.dmg"),
            identity("x1ap", "0x02", "Erase", "erase.dmg"),
            identity("x2ap", "0x04", "Erase", "erase.dmg"),
        )
        .into_bytes()
    }

    #[test]
    fn picks_the_full_restore_for_a_board() {
        let m = Manifest::parse(&sample()).unwrap();
        assert_eq!((m.version.as_str(), m.build.as_str()), ("17.7", "21H16"));
        assert_eq!(m.device_classes(), ["x1ap", "x2ap"]);
        let x1 = m.identity("X1AP").unwrap();
        assert_eq!(
            (x1.chip, x1.board, x1.components["OS"].as_str()),
            (0x8010, 2, "erase.dmg")
        );
        assert_eq!(
            m.identity("x2ap").unwrap().components["iBSS"],
            "Firmware/dfu/iBSS.x2ap.im4p"
        );
        assert!(m.identity("x3ap").is_err());
        assert_eq!(m.identity_for(0x8010, 4).unwrap().device_class, "x2ap");
        assert_eq!(m.identity_for(0x8010, 2).unwrap().behavior, "Erase");
    }
}
