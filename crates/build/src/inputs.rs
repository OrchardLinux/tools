//! What a distribution build takes from orchard finished, as one JSON
//! document: per platform its kernel bundle and its boards with their
//! device trees, the shared m1n1, Pongo and static tools, the revision of
//! orchard's data, and under `produced` how each component was got. Paths
//! point into the cache and stay valid for the build.
//!
//! With 32-bit platforms the document is that root's: `"architecture":
//! "arm"`, ARMv7 tools, and per platform the Cascadia loader of each board
//! that has one. Its m1n1 and Pongo are not for its boards: a release's
//! installer carries them for every device, and takes them from here
//! instead of building its own. One document is one root architecture.
use crate::components::{self, cascadia_loader, cascadia_profile, TOOLS};
use crate::{invalid, kernel, platform, published, snapshot, sources, Build};
use orchard_cache::{Code, Context, Error, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;

/// What the document is asked to cover, and what is given finished.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Default: every arm64 platform.
    pub platforms: Vec<String>,
    /// A platform's staged kernel bundle instead of orchard's.
    pub kernels: BTreeMap<String, PathBuf>,
    pub m1n1: Option<PathBuf>,
    pub pongo: Option<PathBuf>,
    /// A directory with the static tools.
    pub tools: Option<PathBuf>,
    /// ARMv7: a board's Cascadia loader.
    pub loaders: BTreeMap<String, PathBuf>,
    /// A developer's kernel tree to build every selected kernel from.
    pub kernel_tree: Option<PathBuf>,
}

/// The commit orchard's data is at, and whether it differs from it.
fn revision(root: &Path) -> Value {
    if root.join(".git").exists() {
        let commit = sources::git(root, &["rev-parse", "HEAD"], &[]);
        let status = sources::git(
            root,
            &["status", "--porcelain", "--untracked-files=no"],
            &[],
        );
        if let (Ok(commit), Ok(status)) = (commit, status) {
            return json!({"commit": commit, "modified": !status.is_empty()});
        }
    }
    match snapshot::record(root) {
        Some(record) => json!({"commit": record["commit"], "modified": false}),
        None => json!({"commit": null, "modified": null}),
    }
}

fn absolute(path: &Path) -> Result<String> {
    Ok(path
        .canonicalize()
        .context(path.display())?
        .to_string_lossy()
        .into_owned())
}

fn tool_paths(tools: &Path) -> Result<Value> {
    let mut paths = Map::new();
    for name in TOOLS {
        let tool = tools.join(name);
        if !tool.is_file() {
            return Err(Error::new(
                Code::NotFound,
                format!("missing component {}", tool.display()),
            ));
        }
        paths.insert(name.to_owned(), json!(absolute(&tool)?));
    }
    Ok(Value::Object(paths))
}

/// A platform's kernel bundle: the one given, one built from the kernel
/// tree, the nightly's when its source key matches, else built here.
fn bundle(build: &Build, name: &str, request: &Request) -> Result<String> {
    if let Some(given) = request.kernels.get(name) {
        return absolute(given);
    }
    let boards = platform::boards(&build.root, name)?;
    let first = boards.keys().next().expect("a platform has a board");
    if let Some(tree) = &request.kernel_tree {
        let tree = kernel::check_tree(build, first, tree)?;
        return absolute(&kernel::prepare(build, first, Some(&tree))?);
    }
    match published::kernel(build, name)? {
        Some(found) => absolute(&found),
        None => {
            kernel::check_recipe(build, first)?;
            absolute(&kernel::prepare(build, first, None)?)
        }
    }
}

/// The inputs document. The caller holds the cache's lock.
pub fn resolve(build: &Build, request: &Request) -> Result<Value> {
    let root = &build.root;
    let known = platform::platforms(root, "arm64")?;
    let armv7 = platform::platforms(root, "arm")?;
    let selected = if request.platforms.is_empty() {
        known.clone()
    } else {
        request.platforms.clone()
    };
    let is_armv7 = |name: &String| armv7.contains(name);
    if !selected.is_empty() && selected.iter().all(is_armv7) {
        return resolve_armv7(build, &selected, request);
    }
    if selected.iter().any(is_armv7) {
        return Err(invalid(
            "one document is one root architecture: choose arm64 or ARMv7 platforms, not both",
        ));
    }
    if !request.loaders.is_empty() {
        return Err(invalid("Cascadia loaders are for ARMv7 platforms"));
    }
    let mut unknown: Vec<&String> = selected
        .iter()
        .filter(|name| !known.contains(name))
        .collect();
    unknown.extend(
        request
            .kernels
            .keys()
            .filter(|name| !selected.contains(name)),
    );
    if !unknown.is_empty() {
        let names: Vec<&str> = unknown.iter().map(|n| n.as_str()).collect();
        return Err(invalid(format!(
            "unknown or unselected platform {}; known: {}",
            names.join(", "),
            known.join(", ")
        )));
    }
    let all_given = request.m1n1.is_some() && request.pongo.is_some() && request.tools.is_some();
    let found = if all_given {
        None
    } else {
        Some(components::shared(build)?)
    };
    let pick = |given: &Option<PathBuf>, index: usize| -> PathBuf {
        given.clone().unwrap_or_else(|| {
            let (pongo, m1n1, tools) = found.clone().expect("resolved when something is not given");
            [pongo, m1n1, tools][index].clone()
        })
    };
    let (pongo, m1n1, tools) = (
        pick(&request.pongo, 0),
        pick(&request.m1n1, 1),
        pick(&request.tools, 2),
    );
    for path in [&m1n1, &pongo] {
        if !path.is_file() {
            return Err(Error::new(
                Code::NotFound,
                format!("missing component {}", path.display()),
            ));
        }
    }
    let mut platforms = Map::new();
    for name in &selected {
        platforms.insert(
            name.clone(),
            json!({"kernel": bundle(build, name, request)?, "boards": platform::boards(root, name)?}),
        );
    }
    Ok(json!({
        "schema": SCHEMA,
        "orchard": revision(root),
        "platforms": platforms,
        "shared": {"m1n1": absolute(&m1n1)?, "pongo": absolute(&pongo)?, "tools": tool_paths(&tools)?},
        "produced": build.explain.to_json(),
    }))
}

/// The ARMv7 root's document: its kernels, its static tools and its
/// boards' Cascadia loaders.
fn resolve_armv7(build: &Build, selected: &[String], request: &Request) -> Result<Value> {
    let root = &build.root;
    if let Some(stray) = request.kernels.keys().find(|name| !selected.contains(name)) {
        return Err(invalid(format!("unselected platform {stray}")));
    }
    let mut boards = BTreeMap::new();
    for name in selected {
        boards.insert(name.clone(), platform::boards(root, name)?);
    }
    let covered = |board: &String| boards.values().any(|rows| rows.contains_key(board));
    if let Some(stray) = request.loaders.keys().find(|board| !covered(board)) {
        return Err(invalid(format!(
            "{stray}: not a board of {}",
            selected.join(", ")
        )));
    }
    let tools = match &request.tools {
        Some(tools) => tools.clone(),
        None => {
            let first = boards[&selected[0]]
                .keys()
                .next()
                .expect("a platform has a board");
            components::tools(build, first)?
        }
    };
    // For the release's installer, which starts every device.
    let m1n1 = match &request.m1n1 {
        Some(given) => given.clone(),
        None => match published::m1n1(build)? {
            Some(found) => found,
            None => components::build_m1n1(build)?,
        },
    };
    let pongo = match &request.pongo {
        Some(given) => given.clone(),
        None => components::pongo(build)?,
    };
    for path in [&m1n1, &pongo] {
        if !path.is_file() {
            return Err(Error::new(
                Code::NotFound,
                format!("missing component {}", path.display()),
            ));
        }
    }
    let mut platforms = Map::new();
    for name in selected {
        let mut loaders = Map::new();
        for board in boards[name].keys() {
            let loader = match request.loaders.get(board) {
                Some(given) => given.clone(),
                None if cascadia_profile(root, board).is_ok() => cascadia_loader(build, board)?,
                None => continue,
            };
            if !loader.is_file() {
                return Err(Error::new(
                    Code::NotFound,
                    format!("missing component {}", loader.display()),
                ));
            }
            loaders.insert(board.clone(), json!(absolute(&loader)?));
        }
        platforms.insert(
            name.clone(),
            json!({"kernel": bundle(build, name, request)?, "boards": boards[name], "loaders": loaders}),
        );
    }
    Ok(json!({
        "schema": SCHEMA,
        "orchard": revision(root),
        "architecture": "arm",
        "platforms": platforms,
        "shared": {"m1n1": absolute(&m1n1)?, "pongo": absolute(&pongo)?, "tools": tool_paths(&tools)?},
        "produced": build.explain.to_json(),
    }))
}
