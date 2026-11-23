use crate::args::{DataCommand, Global};
use crate::output::{Output, Report};
use orchard_boards::data::{board_data, loaders};
use orchard_cache::store::Store;
use orchard_cache::Result;
use serde::Serialize;

#[derive(Serialize)]
struct Data {
    dir: String,
    /// `main` for the copy of orchard's main branch, `local` for `--data`.
    source: &'static str,
    commit: Option<String>,
    /// When the copy was last fetched or confirmed unchanged, in Unix seconds.
    checked: Option<u64>,
    /// Which m1n1 and Pongo are current, once that has been read.
    loaders: Option<loaders::Record>,
}

impl Report for Data {
    fn human(&self) -> Vec<u8> {
        let text = match (self.source, &self.commit) {
            ("main", Some(commit)) => format!("orchard main at {commit}\n{}\n", self.dir),
            ("main", None) => format!("orchard main\n{}\n", self.dir),
            _ => format!("local data\n{}\n", self.dir),
        };
        let mut text = text;
        if let Some(record) = &self.loaders {
            text.push_str(&format!("loaders of {}\n", record.release));
            for (name, loader) in &record.loaders {
                text.push_str(&format!("  {name} {}\n", loader.key));
            }
        }
        text.into_bytes()
    }
}

pub fn run(global: &Global, command: Option<&DataCommand>) -> Result<Output> {
    let store = Store::open(global.cache.as_deref())?;
    // An update refreshes the loaders' record with the board data; a look
    // shows the record that is there.
    let (data, loaders) = match command {
        Some(DataCommand::Update) => (board_data::update(&store)?, Some(loaders::update(&store)?)),
        None => (
            board_data::resolve(global.data.as_deref(), &store)?,
            loaders::existing(&store),
        ),
    };
    let copy = data.copy.as_ref();
    Ok(Data {
        dir: data.dir.display().to_string(),
        source: if copy.is_some() { "main" } else { "local" },
        commit: copy.and_then(|c| c.commit.clone()),
        checked: copy.map(|c| c.checked),
        loaders,
    }
    .into())
}
