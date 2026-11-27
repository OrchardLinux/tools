//! A platform layer made here from a developer's kernel tree: what
//! `install --kernel DIR` installs in place of the release's own layer.
//!
//! The kernel is the board's bundle, built from the tree as `build --kernel`
//! builds it; `producers/platform_layer.py` makes the layer from it and the
//! static BusyBox, the same recipe a distribution's release gets its layers
//! from.
//!
//! The layer is kept in the recipe's work directory
//! (`work/platform-layer/PLATFORM-KERNEL`), not as a component: which files
//! decide a layer besides the kernel is the recipe's to know and the data
//! does not state it, so the recipe runs each time, which takes seconds.
use crate::platform::layout;
use crate::{components, invalid, kernel, read_text, Build, Staged};
use orchard_cache::{Code, Context, Error, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// A platform layer on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub platform: String,
    /// The boards it carries a device tree for: the platform's, by the data.
    pub boards: Vec<String>,
    pub kernel_release: String,
    /// The archive, `platform-PLATFORM.tar.gz`.
    pub file: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub expanded_bytes: u64,
}

/// The layer of `board`'s platform with the kernel built from `tree`. The
/// caller holds the cache's lock, and keeps it while the layer is in use.
pub fn build(build: &Build, board: &str, tree: &Path) -> Result<Layer> {
    let platform = layout(&build.root, board)?.0;
    let tree = kernel::check_tree(build, board, tree)?;
    build.say(format!("Kernel from the tree {}", tree.display()));
    let kernel = kernel::prepare(build, board, Some(&tree))?;
    let busybox = components::tools(build, board)?.join("busybox");
    assemble(build, &platform, &kernel, &busybox)
}

/// Runs the recipe on a staged kernel bundle (a kernel component's `kernel`
/// directory) and puts the layer where it is kept.
fn assemble(build: &Build, platform: &str, kernel: &Path, busybox: &Path) -> Result<Layer> {
    // One directory per kernel: another kernel's install keeps its layer.
    let key = kernel
        .parent()
        .and_then(Path::file_name)
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let work = build
        .cache
        .recipe_work("platform-layer", &format!("{platform}-{key}"))?;
    std::fs::create_dir_all(&work).context(work.display())?;
    // The recipe refuses to write over a layer, so it writes beside it.
    let stage = tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(&work)
        .context(work.display())?;
    let _staged = Staged::new(stage.path());
    let record = stage.path().join("receipt.json");
    build.python(
        &build.producer("platform_layer.py"),
        &[
            OsStr::new(platform),
            "--output".as_ref(),
            stage.path().as_os_str(),
            "--kernel-dir".as_ref(),
            kernel.as_os_str(),
            "--busybox".as_ref(),
            busybox.as_os_str(),
            "--receipt".as_ref(),
            record.as_os_str(),
        ],
    )?;
    let bad = |what: &str| Error::new(Code::Format, what.to_owned()).context(record.display());
    let receipt: serde_json::Value =
        serde_json::from_str(&read_text(&record)?).map_err(|e| bad(&e.to_string()))?;
    let name = format!("platform-{platform}.tar.gz");
    if receipt["file"] != name.as_str() {
        return Err(bad(&format!("the recipe did not make {name}")));
    }
    let text = |key: &str| receipt[key].as_str().map(str::to_owned);
    let (Some(sha256), Some(kernel_release), Some(bytes), Some(expanded_bytes)) = (
        text("sha256"),
        text("kernel_release"),
        receipt["bytes"].as_u64(),
        receipt["expanded_bytes"].as_u64(),
    ) else {
        return Err(bad("the layer's receipt lacks sha256, kernel_release, bytes or expanded_bytes"));
    };
    let boards: Vec<String> = receipt["boards"]
        .as_array()
        .map(|boards| boards.iter().filter_map(|b| b.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    if boards.is_empty() {
        return Err(invalid(format!("the layer of {platform} names no board")));
    }
    let made = stage.path().join(&name);
    if std::fs::metadata(&made).context(made.display())?.len() != bytes {
        return Err(bad("the layer's size differs from its receipt"));
    }
    // The same inputs give the same archive, so a layer already there is
    // replaced by its equal, in one step for whoever reads it.
    let file = work.join(&name);
    std::fs::rename(&made, &file).context(file.display())?;
    std::fs::rename(&record, work.join("receipt.json")).context(work.display())?;
    Ok(Layer {
        platform: platform.to_owned(),
        boards,
        kernel_release,
        file,
        bytes,
        sha256: sha256.to_ascii_lowercase(),
        expanded_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::tests::fixture;

    /// The layer recipe, reduced to what this crate sees of it: an archive
    /// named after the platform and a receipt for it.
    const RECIPE: &str = r#"import argparse, hashlib, json, pathlib
parser = argparse.ArgumentParser()
parser.add_argument('platform')
for name in ('--output', '--kernel-dir', '--busybox', '--receipt'):
    parser.add_argument(name, type=pathlib.Path, required=True)
args = parser.parse_args()
data = (args.kernel_dir / 'Image.gz').read_bytes() + args.busybox.read_bytes()
output = args.output / f'platform-{args.platform}.tar.gz'
with output.open('xb') as stream:
    stream.write(data)
args.receipt.write_text(json.dumps({
    'file': output.name, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
    'expanded_bytes': 4 * len(data), 'kernel_release': (args.kernel_dir / 'kernel.release').read_text().strip(),
    'boards': ['aa1', 'bb2']}))
print('{}')
"#;

    fn quiet(_: &str) {}

    #[test]
    fn a_layer_is_made_from_a_kernel_bundle_and_kept_in_the_cache() {
        let Some(fixture) = fixture() else { return };
        std::fs::write(fixture.root.join("producers/platform_layer.py"), RECIPE).unwrap();
        let busybox = fixture.root.join("busybox");
        std::fs::write(&busybox, "busybox").unwrap();
        let build = Build::new(&fixture.root, &fixture.store, &quiet).unwrap();
        let kernel = kernel::prepare(&build, "aa1", None).unwrap();
        let layer = assemble(&build, "wide", &kernel, &busybox).unwrap();
        assert_eq!(layer.platform, "wide");
        assert_eq!(layer.boards, ["aa1", "bb2"]);
        assert_eq!(layer.kernel_release, "6.0-test");
        let data = std::fs::read(&layer.file).unwrap();
        assert_eq!(data, b"int a;\nall:\nbusybox");
        assert_eq!((layer.bytes, layer.expanded_bytes), (data.len() as u64, 4 * data.len() as u64));
        assert_eq!(layer.sha256, crate::json::sha256(&data));
        // In the recipe's work directory, named by the platform and the kernel.
        let work = layer.file.parent().unwrap();
        let key = kernel.parent().unwrap().file_name().unwrap().to_string_lossy();
        assert_eq!(work, build.cache.work().join("platform-layer").join(format!("wide-{key}")));
        // Made again it takes the first one's place, and nothing is left beside it.
        assert_eq!(assemble(&build, "wide", &kernel, &busybox).unwrap(), layer);
        let mut left: Vec<_> = std::fs::read_dir(work).unwrap().map(|item| item.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["platform-wide.tar.gz", "receipt.json"]);
    }

    #[test]
    fn a_layer_needs_a_board_with_a_kernel_and_a_kernel_tree() {
        let Some(fixture) = fixture() else { return };
        let build = Build::new(&fixture.root, &fixture.store, &quiet).unwrap();
        let none = tempfile::tempdir().unwrap();
        // A board the data only identifies, a platform's name, and no tree.
        assert!(super::build(&build, "dd4", none.path()).unwrap_err().message.contains("no kernel yet"));
        assert!(super::build(&build, "wide", none.path()).unwrap_err().message.contains("not a device"));
        let error = super::build(&build, "aa1", none.path()).unwrap_err();
        assert!(error.message.contains("is not a kernel tree"), "{error}");
    }
}
