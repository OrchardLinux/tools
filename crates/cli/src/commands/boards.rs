use crate::args::Global;
use crate::output::{Output, Report};
use orchard_boards::catalog::{Catalog, Kind};
use orchard_cache::Result;
use serde::Serialize;
use std::fmt::Write;

#[derive(Serialize)]
struct Boards {
    boards: Vec<Row>,
}

#[derive(Serialize)]
struct Row {
    name: String,
    display_name: String,
    chip: String,
    status: String,
}

impl Report for Boards {
    fn human(&self) -> Vec<u8> {
        let mut text = String::new();
        for b in &self.boards {
            let _ = writeln!(
                text,
                "{:<8} {:<6} {:<13} {}",
                b.name, b.chip, b.status, b.display_name
            );
        }
        text.into_bytes()
    }
}

pub fn list(global: &Global) -> Result<Output> {
    let catalog = Catalog::open(&global.data_dir()?)?;
    let mut boards = Vec::new();
    for name in catalog.boards()? {
        let b = catalog.board(&name)?;
        let chip = catalog
            .chip(b.chip)
            .map(|c| c.name)
            .unwrap_or_else(|_| format!("{:#x}", b.chip));
        boards.push(Row {
            name: b.name,
            display_name: b.display_name,
            chip,
            status: b.status,
        });
    }
    Ok(Boards { boards }.into())
}

#[derive(Serialize)]
struct Board {
    name: String,
    display_name: String,
    products: Vec<String>,
    chip: Chip,
    board_id: u32,
    status: String,
    chains: Vec<String>,
    platform: Option<String>,
    dtb: Option<String>,
    features: Vec<Feature>,
    boot_firmware: Option<Firmware>,
    peripheral_firmware: Option<Firmware>,
}

#[derive(Serialize)]
struct Chip {
    cpid: u32,
    name: Option<String>,
    stages: Vec<String>,
}

#[derive(Serialize)]
struct Feature {
    key: String,
    label: String,
    state: String,
}

#[derive(Serialize)]
struct Firmware {
    version: String,
    build: String,
    /// Files the recipe makes (peripheral firmware only).
    files: usize,
}

impl Report for Board {
    fn human(&self) -> Vec<u8> {
        let mut t = String::new();
        let chip = self
            .chip
            .name
            .clone()
            .unwrap_or_else(|| "unknown chip".into());
        let _ = writeln!(t, "{} ({})", self.display_name, self.name);
        let _ = writeln!(t, "products  {}", self.products.join(", "));
        let _ = writeln!(
            t,
            "chip      {chip} ({:#x}), board {:#x}",
            self.chip.cpid, self.board_id
        );
        let _ = writeln!(t, "status    {}", self.status);
        let _ = writeln!(t, "chains    {}", self.chains.join(", "));
        if let (Some(p), Some(d)) = (&self.platform, &self.dtb) {
            let _ = writeln!(t, "kernel    {p}, {d}.dtb");
        }
        for (label, fw) in [
            ("boot fw  ", &self.boot_firmware),
            ("periph fw", &self.peripheral_firmware),
        ] {
            if let Some(f) = fw {
                let files = if f.files > 0 {
                    format!(", {} files", f.files)
                } else {
                    String::new()
                };
                let _ = writeln!(t, "{label} iOS {} ({}){files}", f.version, f.build);
            }
        }
        let _ = writeln!(t, "\nfeatures");
        for f in &self.features {
            let _ = writeln!(t, "  {:<24} {}", f.label, f.state);
        }
        t.into_bytes()
    }
}

pub fn show(global: &Global, name: &str) -> Result<Output> {
    let catalog = Catalog::open(&global.data_dir()?)?;
    let b = catalog.board(name)?;
    let chip = catalog.chip(b.chip).ok();
    // Features in the shared display order, then any the list does not know.
    let mut features = Vec::new();
    for f in catalog.features()? {
        if let Some((_, state)) = b.features.iter().find(|(k, _)| *k == f.key) {
            features.push(Feature {
                key: f.key,
                label: f.label,
                state: state.clone(),
            });
        }
    }
    for (key, state) in &b.features {
        if !features.iter().any(|f| f.key == *key) {
            features.push(Feature {
                key: key.clone(),
                label: key.clone(),
                state: state.clone(),
            });
        }
    }
    let firmware = |kind| {
        b.recipe(kind).ok().map(|r| Firmware {
            version: r.ipsw.version,
            build: r.ipsw.build,
            files: r.outputs.len(),
        })
    };
    let chains = b
        .boot_data()
        .map(|d| d.chains.into_keys().collect())
        .unwrap_or_default();
    Ok(Board {
        boot_firmware: firmware(Kind::Boot),
        peripheral_firmware: firmware(Kind::Peripheral),
        chip: Chip {
            cpid: b.chip,
            name: chip.as_ref().map(|c| c.name.clone()),
            stages: chip.map(|c| c.stages).unwrap_or_default(),
        },
        name: b.name,
        display_name: b.display_name,
        products: b.products,
        board_id: b.board,
        status: b.status,
        chains,
        platform: b.platform,
        dtb: b.dtb,
        features,
    }
    .into())
}
