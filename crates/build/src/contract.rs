//! What the recipes say about themselves: `producers/contract.toml` in the
//! orchard data. The recipes are updated apart from this tool, so what a
//! key is made from is theirs to state: which files decide m1n1, the static
//! tools, Pongo and a Cascadia loader, and the version of the kernel
//! producers. Recipes that state more than this tool knows are refused.
use crate::{invalid, read_text};
use orchard_cache::{Code, Error, Result};
use std::path::Path;

/// In the orchard data.
pub const NAME: &str = "producers/contract.toml";

/// The newest form of the file this tool reads.
pub const SCHEMA: i64 = 1;

/// The newest kernel producers this tool keys and layers the output of:
/// `kernel/build`, `kernel/build-modules`, `kernel/stage-modules.py`,
/// `kernel/series.py` and the build environment. Their text is not part of
/// a key, so a refactor rebuilds nothing; the recipes raise their number
/// when a change alters what they produce.
pub const KERNEL_PRODUCER: u32 = 3; // 3: strip module debug sections from staged outputs

/// Stands for the board's profile in the Cascadia loader's inputs.
const PROFILE: &str = "{profile}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// Part of every kernel key: changes when the producers stage kernels
    /// differently from the same sources.
    pub kernel_producer: u32,
    /// Files and directories that decide orchard's m1n1.
    pub m1n1: Vec<String>,
    /// The same for the static tools.
    pub tools: Vec<String>,
    /// The same for Pongo.
    pub pongo: Vec<String>,
    /// The same for a board's Cascadia loader, its profile as `{profile}`.
    cascadia: Vec<String>,
}

impl Contract {
    /// What decides the Cascadia loader built for `profile`, a path in the
    /// data.
    pub fn cascadia(&self, profile: &str) -> Vec<String> {
        self.cascadia
            .iter()
            .map(|path| match path.as_str() {
                PROFILE => profile.to_owned(),
                _ => path.clone(),
            })
            .collect()
    }
}

/// The contract of the data in `root`.
pub fn read(root: &Path) -> Result<Contract> {
    let path = root.join(NAME);
    if !path.is_file() {
        // A copy of a nightly is refreshed by the tool, a checkout by its owner.
        let hint = match crate::snapshot::record(root) {
            Some(_) => "orchard2 data update fetches orchard's newest recipes",
            None => "update this orchard checkout, or leave --data out to use orchard's nightly",
        };
        return Err(Error::new(
            Code::Unsupported,
            format!("no {NAME}: these recipes are older than this orchard2 builds from"),
        )
        .context(root.display())
        .hint(hint));
    }
    let bad = |what: String| invalid(format!("{}: {what}", path.display()));
    let file: toml::Table = toml::from_str(&read_text(&path)?)
        .map_err(|e| Error::new(Code::Format, e.message().to_owned()).context(path.display()))?;
    let number = |key: &str| {
        file.get(key)
            .and_then(toml::Value::as_integer)
            .ok_or_else(|| bad(format!("{key} must be a number")))
    };
    let schema = number("schema")?;
    if schema > SCHEMA {
        return Err(newer(format!(
            "these recipes state their contract in form {schema}; this orchard2 reads up to {SCHEMA}"
        )));
    }
    let kernel_producer = u32::try_from(number("kernel-producer")?)
        .map_err(|_| bad("kernel-producer must be a version number".into()))?;
    if kernel_producer > KERNEL_PRODUCER {
        return Err(newer(format!(
            "these recipes make kernels of version {kernel_producer}; this orchard2 knows up to {KERNEL_PRODUCER}"
        )));
    }
    let inputs = file
        .get("inputs")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| bad("[inputs] must be a table".into()))?;
    let list = |key: &str| -> Result<Vec<String>> {
        let paths: Option<Vec<String>> =
            inputs
                .get(key)
                .and_then(toml::Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .map(|item| item.as_str().map(str::to_owned))
                        .collect()
                });
        let paths = paths
            .filter(|paths| !paths.is_empty())
            .ok_or_else(|| bad(format!("inputs.{key} must list paths")))?;
        // A key is made from the data's own files, never from what a path
        // leads to outside it.
        for name in &paths {
            let inside = !name.is_empty()
                && !Path::new(name).is_absolute()
                && name.split('/').all(|part| part != "..");
            if !inside && !(key == "cascadia" && name == PROFILE) {
                return Err(bad(format!(
                    "inputs.{key}: {name:?} is not a path in the data"
                )));
            }
            if name != PROFILE && !root.join(name).exists() {
                return Err(bad(format!(
                    "inputs.{key} names {name}, which is not there"
                )));
            }
        }
        Ok(paths)
    };
    let cascadia = list("cascadia")?;
    if !cascadia.iter().any(|path| path == PROFILE) {
        return Err(bad(format!("inputs.cascadia must name {PROFILE}")));
    }
    Ok(Contract {
        kernel_producer,
        m1n1: list("m1n1")?,
        tools: list("tools")?,
        pongo: list("pongo")?,
        cascadia,
    })
}

fn newer(message: String) -> Error {
    Error::new(Code::Unsupported, message).hint("update orchard2")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(contract: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "boot/m1n1/build",
            "boot/tools/make",
            "boot/pongo.py",
            "boot/cascadia/build",
        ] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let path = dir.path().join(NAME);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contract).unwrap();
        dir
    }

    const INPUTS: &str = "[inputs]\nm1n1 = [\"boot/m1n1\"]\ntools = [\"boot/tools\"]\n\
                          pongo = [\"boot/pongo.py\"]\ncascadia = [\"boot/cascadia\", \"{profile}\"]\n";

    #[test]
    fn the_data_states_what_keys_are_made_from() {
        let dir = data(&format!(
            "schema = 1\nkernel-producer = 2\nlater = true\n{INPUTS}"
        ));
        let contract = read(dir.path()).unwrap();
        assert_eq!(contract.kernel_producer, 2);
        assert_eq!(contract.m1n1, ["boot/m1n1"]);
        assert_eq!(
            contract.cascadia("devices/p105/cascadia.toml"),
            ["boot/cascadia", "devices/p105/cascadia.toml"]
        );
    }

    #[test]
    fn recipes_newer_than_the_tool_are_refused() {
        for head in [
            format!("schema = {}\nkernel-producer = 3\n", SCHEMA + 1),
            format!("schema = 1\nkernel-producer = {}\n", KERNEL_PRODUCER + 1),
        ] {
            let dir = data(&format!("{head}{INPUTS}"));
            let error = read(dir.path()).unwrap_err();
            assert_eq!(error.code, Code::Unsupported, "{head}");
        }
    }

    #[test]
    fn a_list_names_only_files_of_the_data() {
        for (from, to) in [
            ("\"boot/m1n1\"", "\"boot/gone\""),
            ("\"boot/m1n1\"", "\"../outside\""),
            ("\"boot/m1n1\"", "\"/etc/passwd\""),
            ("m1n1 = [\"boot/m1n1\"]", "m1n1 = []"),
            (", \"{profile}\"", ""),
        ] {
            let text = format!("schema = 1\nkernel-producer = 3\n{INPUTS}").replace(from, to);
            let dir = data(&text);
            let error = read(dir.path()).unwrap_err();
            assert_eq!(error.code, Code::InvalidInput, "{to}");
        }
    }

    #[test]
    fn recipes_without_the_file_are_refused_with_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let error = read(dir.path()).unwrap_err();
        assert_eq!(error.code, Code::Unsupported);
        assert!(error.hint.as_deref().unwrap().contains("checkout"));
        std::fs::write(
            dir.path().join(crate::snapshot::RECORD),
            "{\"tag\": \"nightly-x\"}",
        )
        .unwrap();
        let error = read(dir.path()).unwrap_err();
        assert!(error
            .hint
            .as_deref()
            .unwrap()
            .contains("orchard2 data update"));
    }
}
