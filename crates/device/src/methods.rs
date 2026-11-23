//! Methods: each one move of a device from one state to another, and
//! picking the methods that take a device to where a command needs it.
//!
//! orchard's `boot-paths.toml` lists the methods a version of orchard knows,
//! in order of preference, with the state each needs and the state it
//! leaves: entries (outside tools), chains (what orchard2 sends to bring up
//! a patched iBoot) and loaders (what starts code on that iBoot or on
//! Pongo). The code holds what each one does, in one table ([`does`]).
//! From where the device is, the first method in data order that applies
//! and leads on to the goal is taken, until the goal is reached.
use crate::host::Connect;
use crate::prep::{Files, Load, MethodSteps, Request};
use crate::steps::Run;
use crate::{deliver, prep};
use orchard_boards::catalog::{Catalog, Chip};
use orchard_boards::pack::Form;
use orchard_cache::{Code, Context, Error, Result};
use orchard_usb::{Device, Mode};
use serde::{Deserialize, Serialize};

/// A state a device can be in on the way to a loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// The boot ROM, untouched.
    Dfu,
    /// The boot ROM after an exploit; its serial string carries a marker.
    PwnedDfu,
    /// A patched iBSS started from a jailbroken iOS.
    Kdfu,
    JailbrokenIos,
    Pongo,
    /// A patched iBoot that takes m1n1.
    PatchedRecovery,
    /// An iBEC that starts the loader it is sent.
    Autogo,
    M1n1,
    Linux,
}

impl State {
    pub const ALL: [State; 9] = [
        State::Dfu,
        State::PwnedDfu,
        State::Kdfu,
        State::JailbrokenIos,
        State::Pongo,
        State::PatchedRecovery,
        State::Autogo,
        State::M1n1,
        State::Linux,
    ];

    /// The name in orchard's data and in output, the same as in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Dfu => "dfu",
            State::PwnedDfu => "pwned-dfu",
            State::Kdfu => "kdfu",
            State::JailbrokenIos => "jailbroken-ios",
            State::Pongo => "pongo",
            State::PatchedRecovery => "patched-recovery",
            State::Autogo => "autogo",
            State::M1n1 => "m1n1",
            State::Linux => "linux",
        }
    }

    pub fn parse(name: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.as_str() == name)
    }

    /// The mode a device in this state is listed in. A running iOS is not
    /// listed at all.
    pub fn mode(self) -> Option<Mode> {
        match self {
            State::Dfu | State::PwnedDfu | State::Kdfu => Some(Mode::Dfu),
            State::PatchedRecovery | State::Autogo => Some(Mode::Recovery),
            State::Pongo => Some(Mode::Pongo),
            State::M1n1 => Some(Mode::M1n1),
            State::Linux => Some(Mode::Linux),
            State::JailbrokenIos => None,
        }
    }

    /// Whether USB tells this state apart from the others of its mode. The
    /// rest are known from the session that left them, or from the user.
    pub fn shown(self) -> bool {
        matches!(
            self,
            State::Dfu | State::PwnedDfu | State::Pongo | State::M1n1 | State::Linux
        )
    }
}

/// Where a device is: its state, and in pwned DFU the marker it shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Place {
    pub state: State,
    /// The pwned-DFU marker, which names the tool that left it.
    pub marker: Option<String>,
}

/// The state `device` is in. USB shows some states; one it does not show is
/// taken from `remembered` (what the session left the device in) or `stated`
/// (what the user says), never guessed. What is stated must fit what USB
/// shows, and counts over what is remembered.
pub fn place(device: &Device, remembered: Option<State>, stated: Option<State>) -> Result<Place> {
    let marker = device.identity.pwned.clone();
    let mode = device.mode;
    let shown = match mode {
        Mode::Dfu if marker.is_some() => Some(State::PwnedDfu),
        Mode::Dfu => Some(State::Dfu),
        // Neither recovery nor a running iOS says more than USB shows.
        Mode::Recovery | Mode::Normal => None,
        Mode::Pongo => Some(State::Pongo),
        Mode::M1n1 => Some(State::M1n1),
        Mode::Linux => Some(State::Linux),
    };
    // A hidden state stands in for the plain one of its mode, never for one
    // USB shows to be something else.
    let fits = |state: State| {
        state.mode() == Some(mode)
            && match shown {
                Some(State::PwnedDfu) => state == State::PwnedDfu,
                Some(seen) => state == seen || !state.shown(),
                None => true,
            }
    };
    let state = match stated {
        Some(stated) if fits(stated) => stated,
        Some(stated) => {
            let found = match shown {
                Some(seen) => format!("the device shows {}", seen.as_str()),
                None => format!("the device is in {} mode", mode.as_str()),
            };
            return Err(Error::new(
                Code::InvalidInput,
                format!("the device is not in {}: {found}", stated.as_str()),
            ));
        }
        None => match (remembered.filter(|s| !s.shown() && fits(*s)), shown) {
            (Some(remembered), _) => remembered,
            (None, Some(shown)) => shown,
            (None, None) => {
                let hidden: Vec<String> = State::ALL
                    .into_iter()
                    .filter(|s| s.mode() == Some(mode) && !s.shown())
                    .map(|s| format!("--from {}", s.as_str()))
                    .collect();
                return Err(Error::new(
                    Code::Ambiguous,
                    format!(
                        "the device is in {} mode, which does not show what it runs",
                        mode.as_str()
                    ),
                )
                .hint(format!(
                    "state it with {}, or put the device into DFU mode",
                    hidden.join(" or ")
                )));
            }
        },
    };
    Ok(Place { state, marker })
}

/// What a method is, by the table of `boot-paths.toml` it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An outside tool the session runs.
    Entry,
    /// What orchard2 sends to bring up a patched iBoot.
    Chain,
    /// What starts code on a prepared receiver.
    Loader,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Entry => "entry",
            Kind::Chain => "chain",
            Kind::Loader => "loader",
        }
    }
}

/// One move from one state to another, as orchard's data describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    pub name: String,
    pub kind: Kind,
    pub needs: State,
    pub leaves: State,
    /// Entries: the chips they work on, by a chip file's `soc`, and the
    /// hosts they run on.
    pub chips: Vec<String>,
    pub hosts: Vec<String>,
    /// Entries: the pwned-DFU marker the tool leaves, where one is recorded.
    pub marker: Option<String>,
    /// Chains and loaders: a chip file's `architecture`.
    pub arch: Option<String>,
    /// Chains: the pwned-DFU markers they take; empty for any.
    pub markers: Vec<String>,
}

#[derive(Deserialize)]
struct File {
    schema: u32,
    #[serde(default)]
    entry: toml::Table,
    #[serde(default)]
    chain: toml::Table,
    #[serde(default)]
    loader: toml::Table,
}

#[derive(Deserialize)]
struct EntryData {
    needs: String,
    leaves: String,
    marker: Option<String>,
    chips: Vec<String>,
    hosts: Vec<String>,
}

#[derive(Deserialize)]
struct PartData {
    needs: Vec<String>,
    leaves: String,
    arch: String,
    #[serde(default)]
    markers: Vec<String>,
}

/// The methods of the catalog's `boot-paths.toml`, in data order.
pub fn read(catalog: &Catalog) -> Result<Vec<Method>> {
    let path = catalog.root().join("boot-paths.toml");
    let text = std::fs::read_to_string(&path).context(path.display())?;
    parse(&text).context(path.display())
}

/// Parses `boot-paths.toml` (schema 1): entries, then chains, then loaders,
/// each table in file order. A chain or loader that starts from several
/// states is one method per state, in the order listed.
pub fn parse(text: &str) -> Result<Vec<Method>> {
    let format = |e: toml::de::Error| Error::new(Code::Format, e.message().to_owned());
    let file: File = toml::from_str(text).map_err(format)?;
    orchard_boards::catalog::check_schema(file.schema, 1)?;
    let state = |name: &str| {
        State::parse(name).ok_or_else(|| {
            Error::new(
                Code::Unsupported,
                format!("the boot paths name a state this version does not know: {name}"),
            )
            .hint("update orchard2")
        })
    };
    let mut methods = Vec::new();
    for (name, value) in file.entry {
        let entry: EntryData = value.try_into().map_err(format)?;
        methods.push(Method {
            name,
            kind: Kind::Entry,
            needs: state(&entry.needs)?,
            leaves: state(&entry.leaves)?,
            chips: entry.chips,
            hosts: entry.hosts,
            marker: entry.marker,
            arch: None,
            markers: Vec::new(),
        });
    }
    for (kind, table) in [(Kind::Chain, file.chain), (Kind::Loader, file.loader)] {
        for (name, value) in table {
            let part: PartData = value.try_into().map_err(format)?;
            for needs in &part.needs {
                methods.push(Method {
                    name: name.clone(),
                    kind,
                    needs: state(needs)?,
                    leaves: state(&part.leaves)?,
                    chips: Vec::new(),
                    hosts: Vec::new(),
                    marker: None,
                    arch: Some(part.arch.clone()),
                    markers: part.markers.clone(),
                });
            }
        }
    }
    Ok(methods)
}

/// What this version runs, by method: its steps, run against a session or
/// recorded for a plan, and for a loader the file it takes. Data that names
/// a method from or to other states than these is not run.
#[allow(clippy::type_complexity)]
pub fn does<D: Connect>(
    method: &Method,
) -> Option<(
    fn(&mut Run<'_, D>, &MethodSteps, Option<&Chip>, &Request, &mut Files) -> Result<()>,
    Option<Form>,
)> {
    use State::*;
    let name = method.name.as_str();
    Some(match (method.kind, name, method.needs, method.leaves) {
        (Kind::Entry, "palera1n" | "checkra1n", Dfu, Pongo) => (prep::entry::<D>, None),
        (Kind::Entry, "gaster", Dfu, PwnedDfu) => (prep::entry::<D>, None),
        (Kind::Chain, "iboot", PwnedDfu, PatchedRecovery) => (prep::iboot::<D>, None),
        (Kind::Chain, "iboot-autogo", PwnedDfu | Kdfu, Autogo) => (prep::autogo::<D>, None),
        (Kind::Loader, "m1n1", Pongo, M1n1) => (deliver::pongo::<D>, Some(Form::Image)),
        (Kind::Loader, "m1n1", PatchedRecovery, M1n1) => (deliver::restore::<D>, Some(Form::MachO)),
        (Kind::Loader, "cascadia", Autogo, Linux) => (deliver::autogo::<D>, Some(Form::Cascadia)),
        _ => return None,
    })
}

/// The host's name in an entry's `hosts`.
pub fn host() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Whether a loader that takes `form` starts what the command loads; where
/// not, why, and what the user can do instead.
fn wanted(
    load: Option<&Load>,
    loader: &Method,
    form: Form,
) -> std::result::Result<(), (String, Option<String>)> {
    match load {
        None => Ok(()),
        Some(Load::File(file, _)) if Form::of(file) == form => Ok(()),
        Some(Load::File(file, _)) => Err((
            format!(
                "takes {}, and the file is {}",
                form.as_str(),
                Form::of(file).as_str()
            ),
            None,
        )),
        Some(Load::M1n1) if loader.leaves == State::M1n1 => Ok(()),
        // The 32-bit boards have no m1n1 to boot an installed system
        // through; their loader starts the kernel it is given.
        Some(Load::M1n1) => Err((
            format!("starts {}, not m1n1", loader.leaves.as_str()),
            (loader.leaves == State::Linux).then(|| {
                "start Linux on this board with `orchard2 load LOADER --kernel FILE --dtb FILE --initramfs FILE`"
                    .to_owned()
            }),
        )),
    }
}

/// Why `method` cannot run from here with `marker`, as one line, and whether
/// it is for another chip; `None` when it can. What the user can do about
/// it goes to `hints`.
fn why_not(
    method: &Method,
    marker: Option<&str>,
    chip: &Chip,
    refused: &[(String, String)],
    load: Option<&Load>,
    hints: &mut Vec<String>,
) -> Option<(String, bool)> {
    let mut hint = |hint: String| {
        if !hints.contains(&hint) {
            hints.push(hint);
        }
    };
    let line = |why: &[String]| format!("{}: {}", method.name, why.join(", "));
    // A method for another chip says that alone.
    let other = match method.kind {
        Kind::Entry => !method.chips.contains(&chip.soc),
        _ => method.arch.as_deref() != Some(chip.architecture.as_str()),
    };
    if other {
        let why = match method.kind {
            Kind::Entry => format!("does not work on the {}", chip.name),
            _ => format!("is for {} chips", method.arch.as_deref().unwrap_or("other")),
        };
        return Some((line(&[why]), true));
    }
    let mut why = Vec::new();
    if method.kind == Kind::Entry {
        if !method.hosts.iter().any(|h| h == host()) {
            why.push(format!("does not run on {}", host()));
        }
        if let Some(Err(e)) = crate::entry::named(&method.name).map(|e| e.available()) {
            why.push(e.message);
        }
    }
    if let Some((_, reason)) = refused.iter().find(|(name, _)| *name == method.name) {
        why.push(reason.clone());
    }
    if method.kind == Kind::Chain && method.needs == State::PwnedDfu {
        match marker {
            Some(marker)
                if !method.markers.is_empty() && !method.markers.iter().any(|m| m == marker) =>
            {
                why.push(format!("does not take the {marker} marker"))
            }
            Some(marker) if prep::start(marker).is_none() => {
                why.push(format!("no pwned-DFU start for the {marker} marker"))
            }
            Some(_) => {}
            None => {
                why.push("the pwned-DFU start needs the device's marker".into());
                hint("connect the pwned device and plan without --from".into());
            }
        }
    }
    match does::<orchard_usb::Host>(method) {
        None => {
            why.push(match method.kind {
                Kind::Entry => "not run by this version".to_owned(),
                _ => format!(
                    "not run by this version from {} to {}",
                    method.needs.as_str(),
                    method.leaves.as_str()
                ),
            });
            // What such an entry leaves, a chain can still start from.
            if method.kind == Kind::Entry && method.leaves == State::PwnedDfu {
                hint(format!(
                    "pwn the device with {}, reconnect it to this host, then run again",
                    method.name
                ));
            }
        }
        Some((_, Some(form))) if method.kind == Kind::Loader => {
            if let Err((reason, then)) = wanted(load, method, form) {
                why.push(reason);
                if let Some(then) = then {
                    hint(then);
                }
            }
        }
        Some(_) => {}
    }
    (!why.is_empty()).then(|| (line(&why), false))
}

/// Picks the methods that take a device from `from` to a receiver, then the
/// loader that starts what `request` loads there: depth first, the first
/// method in data order that applies and leads on. `refused` names the
/// methods the board cannot take, with the reason. When nothing fits, the
/// error says per considered method why it does not apply.
pub fn pick(
    methods: &[Method],
    from: &Place,
    chip: Option<&Chip>,
    refused: &[(String, String)],
    request: &Request,
) -> Result<Vec<MethodSteps>> {
    let start = from.state.as_str();
    if let Some(wanted) = request.entry {
        let entry = named(methods, Kind::Entry, wanted)?;
        if entry.needs != from.state {
            return Err(Error::new(
                Code::InvalidInput,
                format!(
                    "{wanted} starts from {}, and the device is in {start}",
                    entry.needs.as_str()
                ),
            ));
        }
    }
    if let Some(wanted) = request.chain {
        named(methods, Kind::Chain, wanted)?;
    }
    let taken = |method: &Method, marker: Option<String>| MethodSteps {
        method: method.clone(),
        marker,
        steps: Vec::new(),
        done: None,
    };
    let Some(chip) = chip else {
        // Without a board only a loader that needs no board facts applies.
        let ready = methods.iter().find(|m| {
            m.kind == Kind::Loader
                && m.needs == from.state
                && request.entry.is_none()
                && request.chain.is_none()
                && does::<orchard_usb::Host>(m).is_some_and(|(_, form)| {
                    form == Some(Form::Image)
                        && wanted(request.load.as_ref(), m, Form::Image).is_ok()
                })
        });
        return match ready {
            Some(method) => Ok(vec![taken(method, None)]),
            None => Err(Error::new(
                Code::NotFound,
                format!("a device in {start} does not say which board it is"),
            )
            .hint("name the board with --board, or follow the device from DFU mode in one `orchard2 session`")),
        };
    };
    // An offline plan may take the marker from the catalog only when the
    // entries for this chip agree on it. A connected device's marker wins.
    let marker = from.marker.clone().or_else(|| {
        if from.state != State::PwnedDfu {
            return None;
        }
        let mut markers = methods
            .iter()
            .filter(|m| {
                m.kind == Kind::Entry && m.leaves == State::PwnedDfu && m.chips.contains(&chip.soc)
            })
            .map(|m| m.marker.clone());
        let first = markers.next().flatten()?;
        markers.all(|m| m.as_ref() == Some(&first)).then_some(first)
    });
    let load = request.load.as_ref();
    // One line per method that does not apply, and whether it is for
    // another chip.
    let mut lines: Vec<(String, bool)> = Vec::new();
    let mut hints = Vec::new();
    let mut lacks = |method: &Method, marker: Option<&str>| {
        let Some((line, other)) = why_not(method, marker, chip, refused, load, &mut hints) else {
            return false;
        };
        if !lines.iter().any(|(l, _)| *l == line) {
            lines.push((line, other));
        }
        true
    };
    // Depth first: the method to try next, the marker it starts with, and
    // the methods taken before it.
    let mut stack = vec![(None::<&Method>, marker, Vec::new())];
    while let Some((next, marker, mut path)) = stack.pop() {
        let (state, marker) = match next {
            None => (from.state, marker),
            Some(method) => {
                let asked = match method.kind {
                    Kind::Entry => request.entry,
                    _ => request.chain,
                };
                if asked.is_some_and(|name| name != method.name) || lacks(method, marker.as_deref())
                {
                    continue;
                }
                let next = match method.leaves {
                    State::PwnedDfu => method.marker.clone().or(marker.clone()),
                    _ => None,
                };
                path.push(taken(method, marker));
                (method.leaves, next)
            }
        };
        if path.len() > methods.len() {
            continue;
        }
        // Where a loader starts, the device is prepared, with the entry and
        // chain asked for behind it.
        let behind = |kind: Kind| path.iter().any(|m: &MethodSteps| m.method.kind == kind);
        if (request.entry.is_none() || behind(Kind::Entry))
            && (request.chain.is_none() || behind(Kind::Chain))
        {
            for method in methods
                .iter()
                .filter(|m| m.kind == Kind::Loader && m.needs == state)
            {
                if !lacks(method, marker.as_deref()) {
                    path.push(taken(method, marker));
                    return Ok(path);
                }
            }
        }
        for method in methods
            .iter()
            .rev()
            .filter(|m| m.kind != Kind::Loader && m.needs == state)
        {
            stack.push((Some(method), marker.clone(), path.clone()));
        }
    }
    if lines.is_empty() {
        let error = Error::new(Code::Unsupported, format!("no method starts from {start}"));
        return Err(match from.state {
            State::Dfu => error,
            _ => error.hint("put the device into DFU mode"),
        });
    }
    // Entries for other chips say nothing while one for this chip is there.
    let own = lines.iter().any(|(_, other)| !other);
    let lines: Vec<&str> = lines
        .iter()
        .filter(|(_, other)| !(own && *other))
        .map(|(line, _)| line.as_str())
        .collect();
    let error = Error::new(
        Code::Unsupported,
        format!("no method leads on from {start}: {}", lines.join("; ")),
    );
    Err(match hints.is_empty() {
        true => error,
        false => error.hint(hints.join("; ")),
    })
}

fn named<'m>(methods: &'m [Method], kind: Kind, wanted: &str) -> Result<&'m Method> {
    let mut names: Vec<&str> = Vec::new();
    for method in methods.iter().filter(|m| m.kind == kind) {
        if method.name == wanted {
            return Ok(method);
        }
        if !names.contains(&method.name.as_str()) {
            names.push(&method.name);
        }
    }
    Err(Error::new(
        Code::NotFound,
        format!("no {} named {wanted} in the boot paths", kind.as_str()),
    )
    .hint(format!(
        "the {} names are: {}",
        kind.as_str(),
        names.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use orchard_usb::Identity;

    const DATA: &str = r#"
schema = 1

[entry.pwner]
needs = "dfu"
leaves = "pwned-dfu"
marker = "gaster"
chips = ["soc64", "soc64b"]
hosts = ["macos", "linux"]

[entry.palera1n]
needs = "dfu"
leaves = "pongo"
chips = ["soc64"]
hosts = ["macos", "linux"]

[entry.dongle]
needs = "dfu"
leaves = "pwned-dfu"
marker = "checkm8"
chips = ["soc32"]
hosts = ["macos", "linux"]

[entry.jailbreak]
needs = "jailbroken-ios"
leaves = "kdfu"
chips = ["soc32"]
hosts = ["macos", "linux"]

[chain.iboot]
needs = ["pwned-dfu"]
leaves = "patched-recovery"
arch = "arm64"
markers = ["gaster", "usbliter8"]

[chain.iboot-autogo]
needs = ["pwned-dfu", "kdfu"]
leaves = "autogo"
arch = "arm"
markers = ["checkm8"]

[loader.m1n1]
needs = ["pongo", "patched-recovery"]
leaves = "m1n1"
arch = "arm64"

[loader.cascadia]
needs = ["autogo"]
leaves = "linux"
arch = "arm"
"#;

    /// The data with the made-up pwner entry named as the one this
    /// version runs.
    fn methods() -> Vec<Method> {
        parse(&DATA.replace("entry.pwner", "entry.gaster")).unwrap()
    }

    /// A pick from `state` on a chip, for a board with recipes for
    /// `chains`, asked for nothing.
    struct Case<'a> {
        from: Place,
        chip: Option<Chip>,
        refused: Vec<(String, String)>,
        request: Request<'a>,
    }

    fn case<'a>(
        catalog: &'a Catalog,
        state: State,
        soc: &str,
        arch: &str,
        chains: &[&str],
    ) -> Case<'a> {
        let refused = ["iboot", "iboot-autogo"]
            .into_iter()
            .filter(|c| !chains.contains(c))
            .map(|c| (c.to_owned(), "no recipe for this board".to_owned()))
            .collect();
        Case {
            from: Place {
                state,
                marker: None,
            },
            chip: Some(Chip {
                name: soc.to_uppercase(),
                soc: soc.into(),
                architecture: arch.into(),
                ..fixture::chip(&[])
            }),
            refused,
            request: Request::new(catalog),
        }
    }

    impl Case<'_> {
        /// The names picked, with the loader's start.
        fn pick_in(&self, methods: &[Method]) -> Result<Vec<String>> {
            let path = pick(
                methods,
                &self.from,
                self.chip.as_ref(),
                &self.refused,
                &self.request,
            )?;
            Ok(path
                .iter()
                .map(|m| match m.method.kind {
                    Kind::Loader => format!("{} from {}", m.method.name, m.method.needs.as_str()),
                    _ => m.method.name.clone(),
                })
                .collect())
        }

        fn pick(&self) -> Result<Vec<String>> {
            self.pick_in(&methods())
        }
    }

    #[test]
    fn state_names_match_json_and_parse_back() {
        for state in State::ALL {
            assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
            assert_eq!(State::parse(state.as_str()), Some(state));
        }
        assert_eq!(State::parse("recovery"), None);
    }

    #[test]
    fn methods_are_read_in_data_order_one_per_state_they_start_from() {
        let methods = parse(DATA).unwrap();
        let names: Vec<String> = methods
            .iter()
            .map(|m| format!("{} {}", m.name, m.needs.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                "pwner dfu",
                "palera1n dfu",
                "dongle dfu",
                "jailbreak jailbroken-ios",
                "iboot pwned-dfu",
                "iboot-autogo pwned-dfu",
                "iboot-autogo kdfu",
                "m1n1 pongo",
                "m1n1 patched-recovery",
                "cascadia autogo",
            ]
        );
        assert_eq!(methods[0].marker.as_deref(), Some("gaster"));
        assert_eq!(methods[6].kind, Kind::Chain);
        assert_eq!(methods[5].markers, ["checkm8"]);

        let e = parse("schema = 2\n").unwrap_err();
        assert_eq!(e.code, Code::Unsupported);
        let e = parse(&DATA.replace(
            "\"autogo\"\narch = \"arm\"\nmarkers",
            "\"elsewhere\"\narch = \"arm\"\nmarkers",
        ))
        .unwrap_err();
        assert!(
            e.message.ends_with("does not know: elsewhere"),
            "{}",
            e.message
        );
    }

    #[test]
    fn the_first_method_in_data_order_that_leads_on_is_taken() {
        let (_dir, catalog) = fixture::catalog();
        // The 64-bit chip: through the worker and its chain to a patched
        // iBoot; the 32-bit one: through the chain to the autogo iBEC.
        let first = case(&catalog, State::Dfu, "soc64", "arm64", &["iboot"]);
        assert_eq!(
            first.pick().unwrap(),
            ["gaster", "iboot", "m1n1 from patched-recovery"]
        );
        // The dongle is not run by this version: its pwned DFU is the start.
        let mut second = case(&catalog, State::PwnedDfu, "soc32", "arm", &["iboot-autogo"]);
        second.from.marker = Some("checkm8".into());
        assert_eq!(
            second.pick().unwrap(),
            ["iboot-autogo", "cascadia from autogo"]
        );
        // Without a recipe the chain does not apply; the next entry leads on.
        let no_recipe = case(&catalog, State::Dfu, "soc64", "arm64", &[]);
        assert_eq!(no_recipe.pick().unwrap(), ["palera1n", "m1n1 from pongo"]);
    }

    #[test]
    fn a_receiver_the_device_waits_in_is_used() {
        let (_dir, catalog) = fixture::catalog();
        let pongo = case(&catalog, State::Pongo, "soc64", "arm64", &["iboot"]);
        assert_eq!(pongo.pick().unwrap(), ["m1n1 from pongo"]);
        let patched = case(
            &catalog,
            State::PatchedRecovery,
            "soc64",
            "arm64",
            &["iboot"],
        );
        assert_eq!(patched.pick().unwrap(), ["m1n1 from patched-recovery"]);
        let kdfu = case(&catalog, State::Kdfu, "soc32", "arm", &["iboot-autogo"]);
        assert_eq!(
            kdfu.pick().unwrap(),
            ["iboot-autogo", "cascadia from autogo"]
        );
        // Asking for a chain the receiver did not come from goes nowhere.
        let mut asked = case(&catalog, State::Pongo, "soc64", "arm64", &["iboot"]);
        asked.request.chain = Some("iboot");
        let e = asked.pick().unwrap_err();
        assert_eq!(e.message, "no method starts from pongo");
        assert_eq!(e.hint.as_deref(), Some("put the device into DFU mode"));
    }

    #[test]
    fn the_file_a_loader_takes_chooses_the_receiver() {
        let (_dir, catalog) = fixture::catalog();
        // A plain image goes to Pongo, past the chain that comes first.
        let mut image = case(&catalog, State::Dfu, "soc64", "arm64", &["iboot"]);
        image.request.load = Some(Load::File(b"plain", None));
        assert_eq!(image.pick().unwrap(), ["palera1n", "m1n1 from pongo"]);
        let mut waiting = case(
            &catalog,
            State::PatchedRecovery,
            "soc64",
            "arm64",
            &["iboot"],
        );
        waiting.request.load = image.request.load;
        let e = waiting.pick().unwrap_err();
        assert_eq!(
            e.message,
            "no method leads on from patched-recovery: m1n1: takes a Mach-O m1n1, and the file is a plain image"
        );
        // Booting takes only a loader that starts m1n1, and says how to
        // start Linux where the loader starts it.
        let mut boot = case(&catalog, State::Autogo, "soc32", "arm", &["iboot-autogo"]);
        boot.request.load = Some(Load::M1n1);
        let e = boot.pick().unwrap_err();
        assert_eq!(
            e.message,
            "no method leads on from autogo: cascadia: starts linux, not m1n1"
        );
        assert!(e.hint.unwrap().starts_with("start Linux on this board"));
    }

    #[test]
    fn entry_and_chain_narrow_the_choice() {
        let (_dir, catalog) = fixture::catalog();
        let mut entry = case(&catalog, State::Dfu, "soc64", "arm64", &["iboot"]);
        entry.request.entry = Some("palera1n");
        assert_eq!(entry.pick().unwrap(), ["palera1n", "m1n1 from pongo"]);
        let mut unknown = case(&catalog, State::Dfu, "soc64", "arm64", &[]);
        unknown.request.entry = Some("nope");
        let e = unknown.pick().unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        assert_eq!(
            e.hint.as_deref(),
            Some("the entry names are: gaster, palera1n, dongle, jailbreak")
        );
        let mut elsewhere = case(&catalog, State::Dfu, "soc32", "arm", &[]);
        elsewhere.request.entry = Some("jailbreak");
        assert_eq!(
            elsewhere.pick().unwrap_err().message,
            "jailbreak starts from jailbroken-ios, and the device is in dfu"
        );
        let mut other = case(&catalog, State::Dfu, "soc64", "arm64", &[]);
        other.request.chain = Some("other");
        let e = other.pick().unwrap_err();
        assert_eq!(e.message, "no chain named other in the boot paths");
    }

    #[test]
    fn nothing_fitting_says_per_method_why() {
        let (_dir, catalog) = fixture::catalog();
        // The second 64-bit chip: the worker fits, and the board has no
        // recipe. Entries for other chips are left out of the reasons.
        let e = case(&catalog, State::Dfu, "soc64b", "arm64", &[])
            .pick()
            .unwrap_err();
        assert_eq!(e.code, Code::Unsupported);
        assert_eq!(
            e.message,
            "no method leads on from dfu: iboot: no recipe for this board"
        );
        // A chip no entry lists hears about each entry.
        let e = case(&catalog, State::Dfu, "soc64c", "arm64", &["iboot"])
            .pick()
            .unwrap_err();
        assert_eq!(
            e.message,
            "no method leads on from dfu: gaster: does not work on the SOC64C; \
             palera1n: does not work on the SOC64C; dongle: does not work on the SOC64C"
        );
        // A recipe this version cannot build, and a host an entry lacks.
        let mut board = case(&catalog, State::Dfu, "soc64", "arm64", &[]);
        board.refused = vec![("iboot".into(), "step-x: unknown patch step".into())];
        let elsewhere = DATA.replace("entry.pwner", "entry.gaster").replace(
            "chips = [\"soc64\"]\nhosts = [\"macos\", \"linux\"]",
            "chips = [\"soc64\"]\nhosts = [\"elsewhere\"]",
        );
        let methods = parse(&elsewhere).unwrap();
        assert_eq!(
            board.pick_in(&methods).unwrap_err().message,
            format!(
                "no method leads on from dfu: iboot: step-x: unknown patch step; \
                 palera1n: does not run on {}",
                host()
            )
        );
        // An entry this version does not run says how to go on by hand.
        let e = case(&catalog, State::Dfu, "soc32", "arm", &["iboot-autogo"])
            .pick()
            .unwrap_err();
        assert!(
            e.message.contains("dongle: not run by this version"),
            "{}",
            e.message
        );
        assert_eq!(
            e.hint.as_deref(),
            Some("pwn the device with dongle, reconnect it to this host, then run again")
        );
        // A loader the board lacks data for.
        let mut without = case(&catalog, State::Autogo, "soc32", "arm", &["iboot-autogo"]);
        without.refused = vec![("cascadia".into(), "no loader data for this board".into())];
        assert_eq!(
            without.pick().unwrap_err().message,
            "no method leads on from autogo: cascadia: no loader data for this board"
        );
    }

    #[test]
    fn a_chain_takes_only_the_markers_it_lists() {
        let (_dir, catalog) = fixture::catalog();
        let mut pwned = case(&catalog, State::PwnedDfu, "soc32", "arm", &["iboot-autogo"]);
        pwned.from.marker = Some("gaster".into());
        assert_eq!(
            pwned.pick().unwrap_err().message,
            "no method leads on from pwned-dfu: iboot-autogo: does not take the gaster marker"
        );
        // A marker no start in the code speaks.
        let methods = parse(&DATA.replace("markers = [\"gaster\", \"usbliter8\"]", "")).unwrap();
        let mut unknown = case(&catalog, State::PwnedDfu, "soc64", "arm64", &["iboot"]);
        unknown.from.marker = Some("other".into());
        assert!(unknown
            .pick_in(&methods)
            .unwrap_err()
            .message
            .contains("iboot: no pwned-DFU start for the other marker"));
    }

    #[test]
    fn an_offline_plan_takes_the_marker_only_when_the_entries_agree() {
        let (_dir, catalog) = fixture::catalog();
        let pwned = case(&catalog, State::PwnedDfu, "soc64", "arm64", &["iboot"]);
        assert_eq!(
            pwned.pick().unwrap(),
            ["iboot", "m1n1 from patched-recovery"]
        );
        let disagreeing = DATA.replace(
            "chips = [\"soc32\"]\nhosts = [\"macos\", \"linux\"]\n\n[entry.jailbreak]",
            "chips = [\"soc64\"]\nhosts = [\"macos\", \"linux\"]\n\n[entry.jailbreak]",
        );
        let e = pwned.pick_in(&parse(&disagreeing).unwrap()).unwrap_err();
        assert!(e
            .message
            .contains("iboot: the pwned-DFU start needs the device's marker"));
        assert_eq!(
            e.hint.as_deref(),
            Some("connect the pwned device and plan without --from")
        );
    }

    #[test]
    fn without_a_board_only_pongo_takes_a_load() {
        let (_dir, catalog) = fixture::catalog();
        let mut pongo = case(&catalog, State::Pongo, "", "", &[]);
        pongo.chip = None;
        assert_eq!(pongo.pick().unwrap(), ["m1n1 from pongo"]);
        let mut recovery = case(&catalog, State::PatchedRecovery, "", "", &[]);
        recovery.chip = None;
        let e = recovery.pick().unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        assert!(e.hint.unwrap().contains("--board"));
    }

    fn device(mode: Mode, serial: &str) -> Device {
        Device {
            mode,
            system: None,
            location: "0x100000".into(),
            product: None,
            identity: Identity::parse(serial),
        }
    }

    #[test]
    fn usb_shows_some_states() {
        let plain = device(Mode::Dfu, "CPID:8010 ECID:00000000000000A1");
        assert_eq!(place(&plain, None, None).unwrap().state, State::Dfu);
        let pwned = device(Mode::Dfu, "CPID:8942 ECID:00000000000000B2 PWND:[checkm8]");
        let found = place(&pwned, None, None).unwrap();
        assert_eq!(found.state, State::PwnedDfu);
        assert_eq!(found.marker.as_deref(), Some("checkm8"));
        assert_eq!(
            place(&device(Mode::Pongo, ""), None, None).unwrap().state,
            State::Pongo
        );
    }

    #[test]
    fn a_hidden_state_is_remembered_or_stated_never_guessed() {
        let recovery = device(Mode::Recovery, "CPID:8010 ECID:00000000000000A1");
        let e = place(&recovery, None, None).unwrap_err();
        assert_eq!(e.code, Code::Ambiguous);
        assert_eq!(
            e.hint.as_deref(),
            Some("state it with --from patched-recovery or --from autogo, or put the device into DFU mode")
        );
        let remembered = place(&recovery, Some(State::PatchedRecovery), None).unwrap();
        assert_eq!(remembered.state, State::PatchedRecovery);
        // What the user states now counts over what the session remembers.
        let both = place(&recovery, Some(State::PatchedRecovery), Some(State::Autogo)).unwrap();
        assert_eq!(both.state, State::Autogo);
        // A state remembered for another mode says nothing about this one.
        let dfu = device(Mode::Dfu, "CPID:8942 ECID:00000000000000B2");
        assert_eq!(
            place(&dfu, Some(State::Autogo), None).unwrap().state,
            State::Dfu
        );
        assert_eq!(
            place(&dfu, None, Some(State::Kdfu)).unwrap().state,
            State::Kdfu
        );
    }

    #[test]
    fn a_stated_state_must_fit_what_usb_shows() {
        let e = place(&device(Mode::Pongo, ""), None, Some(State::Kdfu)).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput);
        assert_eq!(
            e.message,
            "the device is not in kdfu: the device shows pongo"
        );
        let pwned = device(Mode::Dfu, "CPID:8942 PWND:[checkm8]");
        let e = place(&pwned, None, Some(State::Kdfu)).unwrap_err();
        assert_eq!(
            e.message,
            "the device is not in kdfu: the device shows pwned-dfu"
        );
        assert!(place(&device(Mode::Dfu, "CPID:8942"), None, Some(State::PwnedDfu)).is_err());
        let e = place(
            &device(Mode::Recovery, ""),
            None,
            Some(State::JailbrokenIos),
        )
        .unwrap_err();
        assert_eq!(
            e.message,
            "the device is not in jailbroken-ios: the device is in recovery mode"
        );
    }
}
