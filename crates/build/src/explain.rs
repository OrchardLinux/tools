//! What a build ran, as plain commands: the record behind `build --explain`.
//!
//! Every producer goes through [`crate::Build::command`], which reports it
//! here. A cache entry keeps the commands that produced it (`commands.json`
//! inside the entry), so a build that reuses the entry still lists how it
//! was made. Steps that are not a command of their own are notes.
use orchard_cache::{Context, Result};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// Inside a cache entry: how it was produced.
pub const COMMANDS: &str = "commands.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A command, and the variables the build set for it or that it
    /// reads from the build's own environment.
    Run(Vec<String>, Vec<(String, String)>),
    Note(String),
}

#[derive(Default)]
pub struct Explain {
    /// The build being recorded.
    build: RefCell<Vec<Step>>,
    /// Cache entries being produced, innermost last.
    entries: RefCell<Vec<(PathBuf, Vec<Step>)>>,
}

impl Explain {
    fn add(&self, step: Step) {
        for (_, steps) in self.entries.borrow_mut().iter_mut() {
            steps.push(step.clone());
        }
        self.build.borrow_mut().push(step);
    }

    pub fn command(&self, argv: &[String], env: &[(String, String)]) {
        self.add(Step::Run(argv.to_vec(), env.to_vec()));
    }

    pub fn note(&self, text: impl Into<String>) {
        self.add(Step::Note(text.into()));
    }

    /// Collects the commands that produce one cache entry into
    /// `stage/commands.json`. Paths inside the temporary stage are the
    /// entry's own and are named `$ENTRY`.
    pub fn producing<T>(&self, stage: &Path, produce: impl FnOnce() -> Result<T>) -> Result<T> {
        self.entries
            .borrow_mut()
            .push((stage.to_owned(), Vec::new()));
        let result = produce();
        let (path, steps) = self
            .entries
            .borrow_mut()
            .pop()
            .expect("the entry pushed above");
        let value = result?;
        let stage_name = path.to_string_lossy().into_owned();
        let record: Vec<Value> = steps
            .iter()
            .map(|step| to_json(&rewrite(step, &stage_name, "$ENTRY")))
            .collect();
        let file = path.join(COMMANDS);
        let mut text = serde_json::to_string_pretty(&record).expect("steps serialize");
        text.push('\n');
        std::fs::write(&file, text).context(file.display())?;
        Ok(value)
    }

    /// A cache hit: the commands that once produced `target`.
    pub fn reused(&self, name: &str, target: &Path) {
        let recorded = std::fs::read(target.join(COMMANDS))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<Value>>(&bytes).ok());
        let Some(steps) = recorded else {
            self.note(format!(
                "{name}: reused {}; how it was made is not recorded",
                target.display()
            ));
            return;
        };
        self.note(format!(
            "{name}: reused from the cache ({}); it was produced by:",
            target.display()
        ));
        let target = target.to_string_lossy();
        for step in steps.iter().filter_map(from_json) {
            self.add(rewrite(&step, "$ENTRY", &target));
        }
    }

    /// The steps recorded so far.
    pub fn steps(&self) -> Vec<Step> {
        self.build.borrow().clone()
    }

    /// The steps as `inputs` documents list them.
    pub fn to_json(&self) -> Value {
        Value::Array(self.build.borrow().iter().map(to_json).collect())
    }
}

fn rewrite(step: &Step, old: &str, new: &str) -> Step {
    match step {
        Step::Run(argv, env) => Step::Run(
            argv.iter().map(|a| a.replace(old, new)).collect(),
            env.iter()
                .map(|(name, value)| (name.clone(), value.replace(old, new)))
                .collect(),
        ),
        Step::Note(text) => Step::Note(text.replace(old, new)),
    }
}

fn to_json(step: &Step) -> Value {
    match step {
        Step::Run(argv, env) if env.is_empty() => json!({ "run": argv }),
        Step::Run(argv, env) => {
            let env: serde_json::Map<String, Value> = env
                .iter()
                .map(|(name, value)| (name.clone(), json!(value)))
                .collect();
            json!({ "run": argv, "env": env })
        }
        Step::Note(text) => json!({ "note": text }),
    }
}

fn from_json(value: &Value) -> Option<Step> {
    if let Some(argv) = value.get("run").and_then(Value::as_array) {
        let env = value.get("env").and_then(Value::as_object);
        return Some(Step::Run(
            argv.iter()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect(),
            env.into_iter()
                .flatten()
                .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
                .collect(),
        ));
    }
    value
        .get("note")
        .and_then(Value::as_str)
        .map(|t| Step::Note(t.to_owned()))
}

/// The steps as a shell script; the Python the producers ran with is named
/// `PYTHON`.
pub fn script(steps: &[Step], title: &str, python: &Path) -> String {
    let python = python.to_string_lossy();
    let mut lines = vec![
        "#!/bin/sh".to_owned(),
        format!("# {title}"),
        "# What the build ran, in order (orchard build writes this; --explain prints it)."
            .to_owned(),
        "# Every line runs by path, without the orchard tool, with the variables the build"
            .to_owned(),
        "# gave it; a line that fails ends the script.".to_owned(),
        "# Paths are as they were: temporary directories are gone after the build.".to_owned(),
        format!("PYTHON=${{PYTHON:-python3}}  # the build ran {python}"),
        "set -eu".to_owned(),
        String::new(),
    ];
    for step in steps {
        match step {
            Step::Note(text) => lines.extend(text.lines().map(|line| format!("# {line}"))),
            Step::Run(argv, env) => {
                let word = |a: &String| match *a == python {
                    true => "\"$PYTHON\"".to_owned(),
                    false => quote(a),
                };
                let mut words: Vec<String> = env
                    .iter()
                    .map(|(name, value)| format!("{name}={}", word(value)))
                    .collect();
                words.extend(argv.iter().map(word));
                lines.push(words.join(" "));
            }
        }
    }
    lines.join("\n") + "\n"
}

/// A word as a POSIX shell reads it back unchanged.
pub fn quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn an_entry_keeps_its_commands_and_a_reuse_lists_them() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        std::fs::create_dir(&stage).unwrap();
        let first = Explain::default();
        first.note("before");
        first
            .producing(&stage, || {
                first.command(
                    &run(&["make", "-o", &stage.join("out").to_string_lossy()]),
                    &[(
                        "WORK".to_owned(),
                        stage.join("w").to_string_lossy().into_owned(),
                    )],
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(first.steps().len(), 2);
        let text = std::fs::read_to_string(stage.join(COMMANDS)).unwrap();
        assert!(
            text.contains("$ENTRY/out") && !text.contains("before"),
            "{text}"
        );

        // The entry gets its final place; a later build reuses it.
        let entry = dir.path().join("entry");
        std::fs::rename(&stage, &entry).unwrap();
        let second = Explain::default();
        second.reused("tools", &entry);
        let steps = second.steps();
        assert!(matches!(&steps[0], Step::Note(n) if n.contains("reused from the cache")));
        assert_eq!(
            steps[1],
            Step::Run(
                run(&["make", "-o", &entry.join("out").to_string_lossy()]),
                vec![(
                    "WORK".to_owned(),
                    entry.join("w").to_string_lossy().into_owned()
                )]
            )
        );
    }

    #[test]
    fn a_failed_producer_records_nothing_in_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let explain = Explain::default();
        let failed: Result<()> = explain.producing(dir.path(), || {
            explain.command(&run(&["false"]), &[]);
            Err(orchard_cache::Error::new(
                orchard_cache::Code::Build,
                "failed",
            ))
        });
        assert!(failed.is_err() && !dir.path().join(COMMANDS).exists());
    }

    #[test]
    fn the_script_names_python_and_quotes_words() {
        let steps = [
            Step::Note("two\nlines".into()),
            Step::Run(
                run(&["/usr/bin/python3.14", "a b", "it's", "--x=1"]),
                vec![
                    (
                        "ORCHARD_PYTHON".to_owned(),
                        "/usr/bin/python3.14".to_owned(),
                    ),
                    ("WORK".to_owned(), "/w d".to_owned()),
                ],
            ),
        ];
        let text = script(&steps, "orchard build x", Path::new("/usr/bin/python3.14"));
        assert!(text.contains("# two\n# lines\n"), "{text}");
        assert!(
            text.ends_with(
                "ORCHARD_PYTHON=\"$PYTHON\" WORK='/w d' \"$PYTHON\" 'a b' 'it'\"'\"'s' --x=1\n"
            ),
            "{text}"
        );
    }
}
