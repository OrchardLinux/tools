//! The release the installer installs: always the current one, from the
//! index URL its stamp names (stamp.rs). The index pins the release manifest by
//! hash; the manifest pins every file by hash. An installer that cannot read a
//! newer format says it is out of date.
use orchard_cache::{Code, Error, Result};

/// The manifest formats this installer reads.
const SCHEMA: i64 = 2;

/// A file pinned by size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
    /// The unpacked size, for archives and images that state it.
    pub expanded_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub name: String,
    pub boards: Vec<String>,
    pub layer: Receipt,
}

/// The parts of a release manifest the installer uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub name: String,
    pub version: String,
    /// The account user the release creates; the installer asks for its
    /// password and PIN.
    pub account_user: Option<String>,
    /// `[install.account]` as the release states it, for the install plan.
    pub account: Option<serde_json::Value>,
    /// The boot entry's kernel command line, when the release sets one.
    pub cmdline: Option<String>,
    /// The root's package manager (`pacman` or `apk`), for its kernel package.
    pub base: String,
    pub root: Receipt,
    pub root_map: Receipt,
    pub pongo: Receipt,
    pub m1n1: Receipt,
    pub install_tools: Receipt,
    /// The boot logo m1n1 shows, when the release has one.
    pub logo: Option<Receipt>,
    /// The board data the release was built with (orchard's `devices/`,
    /// `chips/`, `platforms/` …), when the release ships it.
    pub board_data: Option<Receipt>,
    pub platforms: Vec<Platform>,
}

impl Release {
    /// The platform that covers a board.
    pub fn platform_for(&self, board: &str) -> Option<&Platform> {
        self.platforms.iter().find(|p| p.boards.iter().any(|b| b == board))
    }
}

fn format_error(message: impl Into<String>) -> Error {
    Error::new(Code::Format, message.into())
}

fn table<'a>(doc: &'a toml::Table, key: &str) -> Result<&'a toml::Table> {
    doc.get(key).and_then(toml::Value::as_table).ok_or_else(|| format_error(format!("no [{key}]")))
}

fn string(doc: &toml::Table, key: &str) -> Result<String> {
    doc.get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format_error(format!("no {key}")))
}

/// Where a manifest's `file` names resolve: the directory the manifest came
/// from, as a URL (`https://…/download/` or `file:///…/`).
pub fn base_of(url: &str) -> &str {
    &url[..url.rfind('/').map_or(0, |i| i + 1)]
}

/// The location of an entry: its `url`, or its `file` name next to the
/// manifest. A name is a plain file name, never a path.
fn location(doc: &toml::Table, base: &str) -> Result<String> {
    if let Some(url) = doc.get("url").and_then(toml::Value::as_str) {
        return Ok(url.to_owned());
    }
    let file = string(doc, "file").map_err(|_| format_error("an entry has neither url nor file"))?;
    let plain = !file.is_empty() && !file.starts_with('.') && !file.contains(['/', '\\']);
    if !plain {
        return Err(format_error(format!("not a plain file name: {file}")));
    }
    Ok(format!("{base}{file}"))
}

fn receipt(doc: &toml::Table, base: &str) -> Result<Receipt> {
    let bytes = doc.get("bytes").and_then(toml::Value::as_integer).ok_or_else(|| format_error("no bytes"))?;
    let sha256 = string(doc, "sha256")?;
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format_error("sha256 is not 64 hex digits"));
    }
    let expanded_bytes = doc.get("expanded_bytes").and_then(toml::Value::as_integer).map(|n| n as u64);
    Ok(Receipt { url: location(doc, base)?, bytes: bytes as u64, sha256: sha256.to_ascii_lowercase(), expanded_bytes })
}

fn parse(text: &str, kind: &str) -> Result<toml::Table> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| format_error(e.message().to_owned()))?;
    if doc.get("schema").and_then(toml::Value::as_integer) != Some(SCHEMA) {
        return Err(format_error("this installer is out of date: download the current one"));
    }
    if doc.get("kind").and_then(toml::Value::as_str) != Some(kind) {
        return Err(format_error(format!("expected a {kind} document")));
    }
    Ok(doc)
}

/// The release manifest the index pins; `base` is where the index came from.
pub fn parse_index(text: &str, base: &str) -> Result<Receipt> {
    receipt(table(&parse(text, "orchard.index")?, "release")?, base)
}

pub fn parse_release(text: &str, base: &str) -> Result<Release> {
    let doc = parse(text, "orchard.install")?;
    let receipt = |doc: &toml::Table| receipt(doc, base);
    let root = table(&doc, "root")?;
    let mut platforms = Vec::new();
    for (name, row) in table(&doc, "platforms")? {
        let row = row.as_table().ok_or_else(|| format_error("a platform is not a table"))?;
        let boards = row
            .get("boards")
            .and_then(toml::Value::as_array)
            .map(|a| a.iter().filter_map(|b| b.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        platforms.push(Platform { name: name.clone(), boards, layer: receipt(table(row, "layer")?)? });
    }
    let account_user = doc
        .get("install")
        .and_then(|i| i.get("account"))
        .and_then(|a| a.get("user"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let account = doc
        .get("install")
        .and_then(|i| i.get("account"))
        .and_then(|a| serde_json::to_value(a).ok());
    let cmdline = doc.get("cmdline").and_then(toml::Value::as_str).map(str::to_owned);
    let base = doc
        .get("install")
        .and_then(|i| i.get("base"))
        .and_then(toml::Value::as_str)
        .unwrap_or("pacman")
        .to_owned();
    Ok(Release {
        base,
        account,
        cmdline,
        name: string(&doc, "name")?,
        version: string(&doc, "version")?,
        account_user,
        root: receipt(root)?,
        root_map: receipt(table(root, "map")?)?,
        pongo: receipt(table(&doc, "pongo")?)?,
        m1n1: receipt(table(&doc, "m1n1")?)?,
        install_tools: receipt(table(&doc, "install_tools")?)?,
        logo: doc.get("logo").and_then(toml::Value::as_table).map(receipt).transpose()?,
        board_data: doc.get("board_data").and_then(toml::Value::as_table).map(receipt).transpose()?,
        platforms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "2e5e307cf71da8ac6222507d7f61a044cfe2e04ff6d31bb998b44a9c19a16f51";

    fn r(name: &str) -> String {
        format!("\"url\" = \"https://example/{name}\"\n\"bytes\" = 10\n\"sha256\" = \"{H}\"\n")
    }

    fn release() -> String {
        format!(
            "\"schema\" = 2\n\"kind\" = \"orchard.install\"\n\"name\" = \"Omarchy A\"\n\"version\" = \"0.3.2\"\n\
             [install.account]\n\"user\" = \"tablet\"\n\
             [root]\n{}[root.map]\n{}[pongo]\n{}[m1n1]\n{}[install_tools]\n{}\
             [platforms.\"apple-16k\"]\n\"boards\" = [\"j71b\", \"j72b\"]\n[platforms.\"apple-16k\".layer]\n{}",
            r("root.blocks.zst"),
            r("root.map.json"),
            r("pongo.bin"),
            r("m1n1.bin"),
            r("install-tools.tar.gz"),
            r("platform-apple-16k.tar.gz")
        )
    }

    #[test]
    fn index_pins_the_release_manifest() {
        let text = format!("\"schema\" = 2\n\"kind\" = \"orchard.index\"\n[release]\n{}", r("release.toml"));
        assert_eq!(parse_index(&text, "https://x/").unwrap().url, "https://example/release.toml");
    }

    #[test]
    fn release_lists_every_file_and_the_boards_platform() {
        let rel = parse_release(&release(), "https://x/").unwrap();
        assert_eq!((rel.name.as_str(), rel.version.as_str()), ("Omarchy A", "0.3.2"));
        assert_eq!(rel.account_user.as_deref(), Some("tablet"));
        assert_eq!(rel.platform_for("j71b").unwrap().layer.url, "https://example/platform-apple-16k.tar.gz");
        assert!(rel.platform_for("j71").is_none());
    }

    #[test]
    fn a_newer_format_says_the_installer_is_out_of_date() {
        let text = release().replace("\"schema\" = 2", "\"schema\" = 3");
        assert!(parse_release(&text, "https://x/").unwrap_err().to_string().contains("out of date"));
    }

    #[test]
    fn file_names_resolve_next_to_the_manifest() {
        let text = release().replace("\"url\" = \"https://example/pongo.bin\"", "\"file\" = \"pongo.bin\"");
        let rel = parse_release(&text, "file:///builds/dev/").unwrap();
        assert_eq!(rel.pongo.url, "file:///builds/dev/pongo.bin");
        let bad = release().replace("\"url\" = \"https://example/pongo.bin\"", "\"file\" = \"../pongo.bin\"");
        assert!(parse_release(&bad, "file:///builds/dev/").is_err());
        assert_eq!(base_of("https://h/a/b/index.toml"), "https://h/a/b/");
    }

    #[test]
    fn board_data_is_optional_and_pinned_like_the_rest() {
        assert!(parse_release(&release(), "https://x/").unwrap().board_data.is_none());
        let with = format!("{}[board_data]\n{}", release(), r("board-data.tar.gz"));
        let rel = parse_release(&with, "https://x/").unwrap();
        assert_eq!(rel.board_data.unwrap().url, "https://example/board-data.tar.gz");
    }

    #[test]
    fn a_broken_hash_is_refused() {
        assert!(parse_release(&release().replacen(H, "abc", 1), "https://x/").is_err());
    }
}
