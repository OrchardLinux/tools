//! Build a board's boot or peripheral firmware into a directory.
use crate::catalog::{Board, Catalog, Kind};
use crate::data::firmware::Inputs;
use crate::{boot_firmware, peripheral};
use orchard_cache::store::{self, Store};
use orchard_cache::{Code, Context, Error, Result};
use serde::Serialize;
use std::fs::File;
use std::path::Path;

/// What to build, with the inputs that kind of build uses.
pub enum Request<'a> {
    Boot(Boot<'a>),
    Peripheral {
        /// Build what this version can and list the rest, instead of failing.
        skip_unsupported: bool,
    },
}

/// The inputs a boot-firmware build may take.
#[derive(Clone, Copy, Default)]
pub struct Boot<'a> {
    /// Boot chain; the board's sole chain when `None`.
    pub chain: Option<&'a str>,
    /// Explicit firmware-key override; automatic resolution when absent.
    pub keys: Option<&'a Path>,
    /// IM4M manifest; the bundled chip manifest when `None`.
    pub im4m: Option<&'a Path>,
}

#[derive(Serialize)]
pub struct Built {
    pub board: String,
    pub kind: Kind,
    /// The boot chain used; absent for peripheral builds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    pub out: String,
    pub written: Vec<Written>,
    /// Peripheral outputs not yet checked against real inputs, so not built.
    pub pending: Vec<String>,
    /// Peripheral outputs needing a step this version lacks.
    pub unsupported: Vec<Unsupported>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Written {
    pub destination: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct Unsupported {
    pub destination: String,
    pub step: String,
}

pub fn build(
    data: &Path,
    cache: Option<&Path>,
    name: &str,
    out: &Path,
    request: &Request<'_>,
) -> Result<Built> {
    build_with_device(data, cache, name, out, request, None)
}

/// A connected device that decrypts the keybags a boot-firmware build lacks.
/// The device side implements it; this crate never opens USB.
pub trait KeyDevice {
    /// Returns the IV and key of each bag, in order. Called at most once per
    /// build, only when keys are missing and the output directory is
    /// writable.
    fn decrypt(
        &mut self,
        catalog: &Catalog,
        board: &Board,
        store: &Store,
        bags: &[[u8; 48]],
    ) -> Result<Vec<[u8; 48]>>;
}

/// Optional authorization to acquire missing keys; a cache hit never asks
/// the device.
pub fn build_with_device(
    data: &Path,
    cache: Option<&Path>,
    name: &str,
    out: &Path,
    request: &Request<'_>,
    device: Option<&mut dyn KeyDevice>,
) -> Result<Built> {
    if device.is_some() && matches!(request, Request::Peripheral { .. }) {
        return Err(Error::new(
            Code::InvalidInput,
            "--device-keys is only for boot firmware",
        ));
    }
    let catalog = Catalog::open(data)?;
    let board = catalog.board(name)?;
    let mut report = Built {
        board: board.name.clone(),
        kind: match request {
            Request::Boot(_) => Kind::Boot,
            Request::Peripheral { .. } => Kind::Peripheral,
        },
        chain: None,
        out: out.display().to_string(),
        written: Vec::new(),
        pending: Vec::new(),
        unsupported: Vec::new(),
    };
    match request {
        Request::Boot(boot) => {
            let plan = boot_firmware::Plan::new(&catalog, &board, boot.chain)?;
            let store = Store::open(cache)?;
            if device.is_none() {
                report.written = plan.build(&board, &store, boot, out)?;
                report.chain = Some(plan.chain().to_owned());
                return Ok(report);
            }
            let mut prepared = plan.prepare(&board, &store, boot)?;
            if let Some(device) = device.filter(|_| prepared.needs_device()) {
                prepared.check_acquisition()?;
                std::fs::create_dir_all(out).context(out.display())?;
                let _writable = tempfile::NamedTempFile::new_in(out).context(out.display())?;
                prepared.acquire(|bags| device.decrypt(&catalog, &board, &store, bags))?;
            }
            report.written = prepared
                .finish(&store)?
                .iter()
                .map(|(name, bytes)| write(out, name, bytes))
                .collect::<Result<_>>()?;
            report.chain = Some(plan.chain().to_owned());
        }
        Request::Peripheral { skip_unsupported } => {
            let recipe = board.recipe(Kind::Peripheral)?;
            let plan = peripheral::Plan::new(&recipe)?;
            if !skip_unsupported && !plan.unsupported.is_empty() {
                let mut steps: Vec<_> = plan.unsupported.iter().map(|(_, step)| *step).collect();
                steps.sort_unstable();
                steps.dedup();
                return Err(Error::new(
                    Code::Unsupported,
                    format!(
                        "{} needs steps this version cannot do: {}",
                        board.name,
                        steps.join(", ")
                    ),
                ));
            }
            let store = Store::open(cache)?;
            report.written = plan.build(&mut Inputs::new(&store, &board, &recipe), out)?;
            report.pending = plan.pending.iter().map(|s| s.to_string()).collect();
            report.unsupported = plan
                .unsupported
                .iter()
                .map(|(d, s)| Unsupported {
                    destination: d.to_string(),
                    step: s.to_string(),
                })
                .collect();
        }
    }
    Ok(report)
}

/// Writes `bytes` to `destination` below `root`, atomically and world-readable,
/// and returns the record of it. Every builder ends through here.
pub(crate) fn write(root: &Path, destination: &str, bytes: &[u8]) -> Result<Written> {
    let path = root.join(destination);
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent).context(parent.display())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).context(parent.display())?;
    use std::io::Write;
    file.write_all(bytes).context(path.display())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .context(path.display())?;
    }
    file.persist(&path)
        .map_err(|e| Error::from(e.error).context(path.display()))?;
    Ok(Written {
        destination: destination.into(),
        size: bytes.len() as u64,
        sha256: store::sha256(bytes),
    })
}

/// Reads an already-opened file, failing if it holds more than `limit` bytes.
/// `what` names it in any error.
pub(crate) fn read_capped(file: File, limit: u64, what: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .context(what.display())?;
    if bytes.len() as u64 > limit {
        return Err(
            Error::new(Code::TooLarge, "file exceeds its size limit").context(what.display())
        );
    }
    Ok(bytes)
}
