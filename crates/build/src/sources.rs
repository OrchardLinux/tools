//! Upstream Git objects, shared by every source checkout, and the identity
//! of a source tree: the Git tree its content would be committed as.
//!
//! Only objects are shared. An existing checkout is never reset. Cached
//! revisions are kept by refs, since checkouts borrow the store's objects.
use crate::output;
use orchard_cache::component::{lock, Lock};
use orchard_cache::{Code, Context, Error, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn command(repository: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(repository);
    command
}

/// Runs git in `repository` for its output, without the final line end.
pub fn git<S: AsRef<OsStr>>(
    repository: &Path,
    args: &[S],
    env: &[(&str, &OsStr)],
) -> Result<String> {
    let mut command = command(repository);
    command.args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    Ok(String::from_utf8_lossy(&output(&mut command)?)
        .trim()
        .to_owned())
}

/// Whether git in `repository` succeeds with `args`.
pub fn git_ok<S: AsRef<OsStr>>(repository: &Path, args: &[S]) -> bool {
    let _running = crate::Running::new();
    command(repository)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A path git reports, such as its index or object directory, which is
/// relative to the checkout unless it is absolute.
pub fn git_path(source: &Path, name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(git(source, &["rev-parse", "--git-path", name], &[])?);
    Ok(if path.is_absolute() {
        path
    } else {
        source.join(path)
    })
}

/// The bare object store that holds `commit` of `url`, fetched only when
/// the commit is absent. A `donor` checkout that has the commit is fetched
/// from instead of the network. The store stays locked while the returned
/// lock lives.
pub fn repository(
    git_cache: &Path,
    url: &str,
    commit: &str,
    donor: Option<&Path>,
    say: &dyn Fn(&str),
) -> Result<(PathBuf, Lock)> {
    std::fs::create_dir_all(git_cache).context(git_cache.display())?;
    let name = crate::json::sha256(url.as_bytes());
    let bare = git_cache.join(format!("{name}.git"));
    let held = lock(&git_cache.join(format!("{name}.lock")), true, &|| {})?;
    if !bare.exists() {
        output(
            Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&bare),
        )?;
    }
    let has = |repository: &Path| {
        git_ok(
            repository,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        )
    };
    if !has(&bare) {
        let mut origin = url.to_owned();
        if let Some(donor) = donor.filter(|d| d.join(".git").exists() && has(d)) {
            origin = format!(
                "file://{}",
                donor.canonicalize().context(donor.display())?.display()
            );
        }
        say(&format!(
            "Caching upstream Git revision {commit} from {origin}"
        ));
        let status = command(&bare)
            .args([
                "fetch",
                "--progress",
                "--no-tags",
                "--depth",
                "1",
                &origin,
                commit,
            ])
            .status()
            .map_err(|e| Error::new(Code::NotFound, e.to_string()).context("git"))?;
        if !status.success() {
            return Err(Error::new(
                Code::Network,
                format!("git could not fetch {commit} from {origin}"),
            ));
        }
    }
    git(
        &bare,
        &[
            "update-ref",
            &format!("refs/heads/orchard/{commit}"),
            commit,
        ],
        &[],
    )?;
    Ok((bare, held))
}

/// Checks `commit` of `url` out at `source`, which must not exist. The
/// checkout borrows the shared store's objects.
pub fn checkout(
    git_cache: &Path,
    url: &str,
    commit: &str,
    source: &Path,
    say: &dyn Fn(&str),
) -> Result<()> {
    if source.exists() {
        return Err(Error::new(
            Code::InvalidInput,
            format!(
                "source directory exists; will not reset it: {}",
                source.display()
            ),
        ));
    }
    let parent = source.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).context(parent.display())?;
    let (bare, _held) = repository(git_cache, url, commit, None, say)?;
    let temporary = tempfile::Builder::new()
        .prefix("source-")
        .tempdir_in(parent)
        .context(parent.display())?;
    let stage = temporary.path().join("source");
    output(Command::new("git").args(["init", "-q"]).arg(&stage))?;
    // clone --shared does not reliably borrow objects from shallow
    // repositories; explicit alternates keep one shared object store.
    let info = stage.join(".git/objects/info");
    std::fs::create_dir_all(&info).context(info.display())?;
    crate::write_text(
        &info.join("alternates"),
        &format!("{}\n", bare.join("objects").display()),
    )?;
    if bare.join("shallow").exists() {
        std::fs::copy(bare.join("shallow"), stage.join(".git/shallow")).context(stage.display())?;
    }
    git(&stage, &["remote", "add", "origin", url], &[])?;
    git(&stage, &["checkout", "-q", "--detach", commit], &[])?;
    std::fs::rename(&stage, source).context(source.display())
}

/// The tree `commit` becomes with `patches` applied, without a checkout.
pub fn declared_tree(
    git_cache: &Path,
    url: &str,
    commit: &str,
    patches: &[PathBuf],
    donor: Option<&Path>,
    say: &dyn Fn(&str),
) -> Result<String> {
    let (bare, _held) = repository(git_cache, url, commit, donor, say)?;
    let temporary = tempfile::Builder::new()
        .prefix("orchard-index-")
        .tempdir()?;
    let index = temporary.path().join("index");
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    git(&bare, &["read-tree", commit], &env)?;
    // One apply for the whole series writes Linux's large index once, not
    // once per patch.
    if !patches.is_empty() {
        let mut args: Vec<&OsStr> = vec![
            "apply".as_ref(),
            "--cached".as_ref(),
            "--whitespace=nowarn".as_ref(),
        ];
        args.extend(patches.iter().map(|p| p.as_os_str()));
        git(&bare, &args, &env)?;
    }
    let tree = git(&bare, &["write-tree"], &env)?;
    // Keep the objects the patches made reachable, like upstream's commits.
    git(
        &bare,
        &["update-ref", &format!("refs/orchard/trees/{tree}"), &tree],
        &[],
    )?;
    Ok(tree)
}

/// The tree of a checkout as it is, tracked and untracked files alike,
/// without changing its index.
///
/// `force` also takes files the tree's ignore rules match: orchard's own
/// kernel trees build elsewhere, so nothing in them is rightly ignored, and
/// on a case-insensitive filesystem the kernel's `*.s` rule would drop a new
/// `headsmp.S`. A developer's kernel tree keeps its rules, for its in-tree
/// build output.
pub fn working_tree(source: &Path, force: bool) -> Result<String> {
    let temporary = tempfile::Builder::new()
        .prefix("orchard-index-")
        .tempdir()?;
    let index = temporary.path().join("index");
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    let original = git_path(source, "index")?;
    if original.exists() {
        // The copy keeps the index's time: git hashes a file again when it
        // is not older than the index, so an edit in the same second as the
        // last index write is still seen.
        crate::copy_file(&original, &index)?;
    } else {
        git(source, &["read-tree", "HEAD"], &env)?;
    }
    let mut args = vec!["add", "--all"];
    if force {
        args.push("--force");
    }
    args.extend(["--", "."]);
    git(source, &args, &env)?;
    git(source, &["write-tree"], &env)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn quiet(_: &str) {}

    /// A repository with one commit; returns its URL and the commit.
    pub fn upstream(dir: &Path) -> (String, String) {
        let source = dir.join("upstream");
        std::fs::create_dir(&source).unwrap();
        output(
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .arg(&source),
        )
        .unwrap();
        std::fs::write(source.join("Makefile"), "all:\n").unwrap();
        std::fs::write(source.join("file.c"), "int a;\n").unwrap();
        git(&source, &["add", "."], &[]).unwrap();
        git(
            &source,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.org",
                "commit",
                "-q",
                "-m",
                "base",
            ],
            &[],
        )
        .unwrap();
        let commit = git(&source, &["rev-parse", "HEAD"], &[]).unwrap();
        (
            format!("file://{}", source.canonicalize().unwrap().display()),
            commit,
        )
    }

    pub const PATCH: &str = "--- a/file.c\n+++ b/file.c\n@@ -1 +1,2 @@\n int a;\n+int b;\n";

    #[test]
    fn a_checkout_borrows_the_store_and_is_never_reset() {
        let dir = tempfile::tempdir().unwrap();
        let (url, commit) = upstream(dir.path());
        let cache = dir.path().join("git");
        let source = dir.path().join("work/source");
        checkout(&cache, &url, &commit, &source, &quiet).unwrap();
        assert_eq!(git(&source, &["rev-parse", "HEAD"], &[]).unwrap(), commit);
        assert_eq!(
            git(&source, &["remote", "get-url", "origin"], &[]).unwrap(),
            url
        );
        assert!(checkout(&cache, &url, &commit, &source, &quiet)
            .unwrap_err()
            .message
            .contains("will not reset"));
    }

    #[test]
    fn the_declared_tree_is_what_applying_the_patches_gives() {
        let dir = tempfile::tempdir().unwrap();
        let (url, commit) = upstream(dir.path());
        let cache = dir.path().join("git");
        let patch = dir.path().join("0001.patch");
        std::fs::write(&patch, PATCH).unwrap();
        let plain = declared_tree(&cache, &url, &commit, &[], None, &quiet).unwrap();
        let patched = declared_tree(
            &cache,
            &url,
            &commit,
            std::slice::from_ref(&patch),
            None,
            &quiet,
        )
        .unwrap();
        assert_ne!(plain, patched);

        let source = dir.path().join("source");
        checkout(&cache, &url, &commit, &source, &quiet).unwrap();
        assert_eq!(working_tree(&source, true).unwrap(), plain);
        git(&source, &["apply", &patch.to_string_lossy()], &[]).unwrap();
        assert_eq!(working_tree(&source, true).unwrap(), patched);
        // The user's index is untouched: the change is still unstaged.
        assert_eq!(
            git(&source, &["diff", "--cached", "--name-only"], &[]).unwrap(),
            ""
        );
    }

    #[test]
    fn untracked_files_count_and_ignored_ones_only_when_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (url, commit) = upstream(dir.path());
        let source = dir.path().join("source");
        checkout(&dir.path().join("git"), &url, &commit, &source, &quiet).unwrap();
        let clean = working_tree(&source, false).unwrap();
        std::fs::write(source.join("new.c"), "int n;\n").unwrap();
        let with_new = working_tree(&source, false).unwrap();
        assert_ne!(clean, with_new);
        std::fs::write(source.join(".git/info/exclude"), "/out/\n").unwrap();
        std::fs::create_dir(source.join("out")).unwrap();
        std::fs::write(source.join("out/built.o"), "o").unwrap();
        assert_eq!(working_tree(&source, false).unwrap(), with_new);
        assert_ne!(working_tree(&source, true).unwrap(), with_new);
    }
}
