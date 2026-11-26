//! Routes: the ways into a device, from orchard's `routes.toml`.
//!
//! A boot route takes a device from DFU mode to a loader that starts Linux;
//! an access route gets an app of ours running inside iOS. Which of them a
//! device has follows from its chip, and for access routes from the iOS
//! version it runs. The order of the file is the preference.
use crate::catalog::{check_schema, Catalog, Chip};
use orchard_cache::{Code, Context, Error, Result};
use serde::Deserialize;

/// What a route is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Boot,
    Access,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Boot => "boot",
            Kind::Access => "access",
        }
    }

    /// The option that names a route of this kind.
    pub fn option(self) -> &'static str {
        match self {
            Kind::Boot => "--boot",
            Kind::Access => "--access",
        }
    }
}

/// An iOS version, such as 17.7.11.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(Vec<u32>);

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let parts: Option<Vec<u32>> = text.trim().split('.').map(|p| p.parse().ok()).collect();
        parts.filter(|p| !p.is_empty()).map(Version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.0.iter().map(u32::to_string).collect();
        f.write_str(&parts.join("."))
    }
}

/// iOS versions from `first` to `last`, both included; no `last`: and later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    pub first: Version,
    pub last: Option<Version>,
}

impl Range {
    fn holds(&self, version: &Version) -> bool {
        *version >= self.first && self.last.as_ref().is_none_or(|last| version <= last)
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.last {
            Some(last) => write!(f, "{} to {last}", self.first),
            None => write!(f, "{} or later", self.first),
        }
    }
}

/// Where a route works: chips by a chip file's `soc`, and the iOS versions
/// on them; no versions: any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fit {
    pub chips: Vec<String>,
    pub ios: Vec<Range>,
}

/// One route, as the data describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub name: String,
    pub kind: Kind,
    /// Boot routes: the loader the device is left in, `m1n1` or `cascadia`.
    pub ends: Option<String>,
    /// Taken only when asked for by name.
    pub named: bool,
    pub fits: Vec<Fit>,
    pub source: String,
}

/// A route for one device: whether it fits, or why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer<'a> {
    pub route: &'a Route,
    pub why_not: Option<String>,
}

/// Every route, each kind in the data's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routes {
    routes: Vec<Route>,
}

#[derive(Deserialize)]
struct File {
    schema: u32,
    #[serde(default)]
    boot: toml::Table,
    #[serde(default)]
    access: toml::Table,
}

#[derive(Deserialize)]
struct RouteData {
    ends: Option<String>,
    #[serde(default)]
    named: bool,
    fits: Vec<FitData>,
    source: String,
}

#[derive(Deserialize)]
struct FitData {
    chips: Vec<String>,
    #[serde(default)]
    ios: Vec<(String, String)>,
}

/// Whether the catalog has a `routes.toml`. Board data older than the file
/// is still in use on computers that fetched it before.
pub fn present(catalog: &Catalog) -> bool {
    catalog.root().join("routes.toml").is_file()
}

/// The routes of the catalog's `routes.toml`.
pub fn read(catalog: &Catalog) -> Result<Routes> {
    let path = catalog.root().join("routes.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new(Code::NotFound, "the board data has no routes (routes.toml)")
                .hint("use newer board data: orchard2 data update, or --data DIR")
        } else {
            Error::from(e)
        }
    });
    parse(&text.context(path.display())?).context(path.display())
}

/// Parses `routes.toml` (schema 1).
pub fn parse(text: &str) -> Result<Routes> {
    let format = |e: toml::de::Error| Error::new(Code::Format, e.message().to_owned());
    let invalid = |message: String| Error::new(Code::Format, message);
    let file: File = toml::from_str(text).map_err(format)?;
    check_schema(file.schema, 1)?;
    let version = |text: &str| {
        Version::parse(text).ok_or_else(|| invalid(format!("not an iOS version: {text:?}")))
    };
    let mut routes = Vec::new();
    for (kind, table) in [(Kind::Boot, file.boot), (Kind::Access, file.access)] {
        for (name, value) in table {
            let data: RouteData = value.try_into().map_err(format)?;
            let mut fits = Vec::new();
            for fit in data.fits {
                let mut ios = Vec::new();
                for (first, last) in &fit.ios {
                    ios.push(Range {
                        first: version(first)?,
                        last: if last.is_empty() {
                            None
                        } else {
                            Some(version(last)?)
                        },
                    });
                }
                fits.push(Fit {
                    chips: fit.chips,
                    ios,
                });
            }
            if kind == Kind::Boot && data.ends.is_none() {
                return Err(invalid(format!(
                    "the boot route {name} does not say where it ends"
                )));
            }
            routes.push(Route {
                name,
                kind,
                ends: data.ends,
                named: data.named,
                fits,
                source: data.source,
            });
        }
    }
    Ok(Routes { routes })
}

impl Route {
    /// None when the route works on `chip` (running `ios`, where the route
    /// depends on it), else why it does not.
    fn why_not(&self, chip: &Chip, ios: Option<&Version>) -> Option<String> {
        let on_chip: Vec<&Fit> = self
            .fits
            .iter()
            .filter(|f| f.chips.contains(&chip.soc))
            .collect();
        if on_chip.is_empty() {
            return Some(format!("does not work on the {} chip", chip.name));
        }
        if on_chip.iter().any(|f| f.ios.is_empty()) {
            return None;
        }
        let ranges: Vec<String> = on_chip
            .iter()
            .flat_map(|f| &f.ios)
            .map(Range::to_string)
            .collect();
        let needs = format!("needs iOS {}", ranges.join(", or "));
        match ios {
            None => Some(format!("{needs}; the device's version is not known")),
            Some(v) if on_chip.iter().flat_map(|f| &f.ios).any(|r| r.holds(v)) => None,
            Some(v) => Some(format!("{needs}; the device runs {v}")),
        }
    }
}

impl Routes {
    /// The route of `kind` called `name`.
    pub fn named(&self, kind: Kind, name: &str) -> Result<&Route> {
        self.routes
            .iter()
            .find(|r| r.kind == kind && r.name == name)
            .ok_or_else(|| {
                let names: Vec<&str> = self
                    .routes
                    .iter()
                    .filter(|r| r.kind == kind)
                    .map(|r| r.name.as_str())
                    .collect();
                Error::new(
                    Code::InvalidInput,
                    format!("no {} route is called {name}", kind.as_str()),
                )
                .hint(format!(
                    "{} takes one of: {}",
                    kind.option(),
                    names.join(", ")
                ))
            })
    }

    /// The routes of `kind` for a device, in order of preference, each with
    /// why it does not fit where it does not.
    pub fn offers(&self, kind: Kind, chip: &Chip, ios: Option<&Version>) -> Vec<Offer<'_>> {
        self.routes
            .iter()
            .filter(|r| r.kind == kind)
            .map(|route| Offer {
                route,
                why_not: route.why_not(chip, ios),
            })
            .collect()
    }

    /// The route to take: the one `asked` for, which must fit, else the
    /// first that fits and is not for asking only.
    pub fn pick(
        &self,
        kind: Kind,
        chip: &Chip,
        ios: Option<&Version>,
        asked: Option<&str>,
    ) -> Result<&Route> {
        let offers = self.offers(kind, chip, ios);
        let names = |_: bool| {
            offers
                .iter()
                .filter(|o| o.why_not.is_none())
                .map(|o| o.route.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if let Some(asked) = asked {
            self.named(kind, asked)?;
            let offer = offers
                .iter()
                .find(|o| o.route.name == asked)
                .expect("a named route is offered");
            return match &offer.why_not {
                None => Ok(offer.route),
                Some(why) => {
                    let error = Error::new(Code::Unsupported, format!("{asked} {why}"));
                    Err(match names(true) {
                        fitting if fitting.is_empty() => error,
                        fitting => error.hint(format!("this device takes: {fitting}")),
                    })
                }
            };
        }
        if let Some(offer) = offers
            .iter()
            .find(|o| o.why_not.is_none() && !o.route.named)
        {
            return Ok(offer.route);
        }
        let whys: Vec<String> = offers
            .iter()
            .filter_map(|o| o.why_not.as_ref().map(|w| format!("{} {w}", o.route.name)))
            .collect();
        let error = Error::new(
            Code::Unsupported,
            format!("no {} route works on this device", kind.as_str()),
        );
        Err(if whys.is_empty() {
            error
        } else {
            error.hint(whys.join("; "))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA: &str = r#"
schema = 1
[boot.first]
ends = "m1n1"
fits = [{ chips = ["c2"] }]
source = "s"
[boot.second]
ends = "m1n1"
fits = [{ chips = ["c1", "c2"] }]
source = "s"
[boot.special]
ends = "cascadia"
named = true
fits = [{ chips = ["c3"] }]
source = "s"
[access.new]
fits = [{ chips = ["c1"], ios = [["15.0", ""]] }, { chips = ["c2"], ios = [["15.0", "17.3.1"], ["26.0", "26.0.1"]] }]
source = "s"
[access.old]
fits = [{ chips = ["c1", "c2"], ios = [["12.0", "14.8.1"]] }]
source = "s"
"#;

    fn chip(soc: &str) -> Chip {
        Chip {
            cpid: 0,
            name: soc.to_uppercase(),
            soc: soc.into(),
            architecture: "arm64".into(),
            stages: vec![],
            image_format: "im4p".into(),
        }
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(v("15.8.3") < v("15.10"));
        assert!(v("17") < v("17.0.1"));
        assert_eq!(Version::parse("17.x"), None);
        assert_eq!(v("17.7.11").to_string(), "17.7.11");
    }

    #[test]
    fn boot_routes_follow_the_chip_in_file_order() {
        let routes = parse(DATA).unwrap();
        assert_eq!(
            routes
                .pick(Kind::Boot, &chip("c2"), None, None)
                .unwrap()
                .name,
            "first"
        );
        assert_eq!(
            routes
                .pick(Kind::Boot, &chip("c1"), None, None)
                .unwrap()
                .name,
            "second"
        );
        assert_eq!(
            routes
                .pick(Kind::Boot, &chip("c2"), None, Some("second"))
                .unwrap()
                .name,
            "second"
        );
        let offers = routes.offers(Kind::Boot, &chip("c1"), None);
        assert_eq!(offers.len(), 3);
        assert_eq!(
            offers[0].why_not.as_deref(),
            Some("does not work on the C1 chip")
        );
        assert_eq!(offers[1].why_not, None);
    }

    #[test]
    fn a_named_route_is_taken_only_when_asked_for() {
        let routes = parse(DATA).unwrap();
        let error = routes
            .pick(Kind::Boot, &chip("c3"), None, None)
            .unwrap_err();
        assert_eq!(error.code, Code::Unsupported);
        let route = routes
            .pick(Kind::Boot, &chip("c3"), None, Some("special"))
            .unwrap();
        assert_eq!(route.ends.as_deref(), Some("cascadia"));
    }

    #[test]
    fn access_routes_follow_the_ios_version() {
        let routes = parse(DATA).unwrap();
        let pick = |soc: &str, ios: &str| {
            routes
                .pick(Kind::Access, &chip(soc), Some(&v(ios)), None)
                .map(|r| r.name.clone())
        };
        assert_eq!(pick("c1", "17.7.11").unwrap(), "new");
        assert_eq!(pick("c1", "12.5.3").unwrap(), "old");
        assert_eq!(pick("c2", "26.0.1").unwrap(), "new");
        assert_eq!(pick("c2", "18.0").unwrap_err().code, Code::Unsupported);
        let asked = routes
            .pick(Kind::Access, &chip("c2"), Some(&v("14.0")), Some("new"))
            .unwrap_err();
        assert_eq!(asked.to_string(), "new needs iOS 15.0 to 17.3.1, or 26.0 to 26.0.1; the device runs 14.0\nthis device takes: old");
        let unknown = routes
            .pick(Kind::Access, &chip("c1"), None, Some("old"))
            .unwrap_err();
        assert!(unknown
            .to_string()
            .ends_with("the device's version is not known"));
    }

    #[test]
    fn an_unknown_name_lists_the_routes() {
        let routes = parse(DATA).unwrap();
        let error = routes
            .pick(Kind::Boot, &chip("c1"), None, Some("nope"))
            .unwrap_err();
        assert_eq!(error.code, Code::InvalidInput);
        assert_eq!(
            error.to_string(),
            "no boot route is called nope\n--boot takes one of: first, second, special"
        );
    }

    #[test]
    fn a_boot_route_must_say_where_it_ends() {
        let error = parse("schema = 1\n[boot.x]\nfits = [{ chips = [\"c1\"] }]\nsource = \"s\"\n")
            .unwrap_err();
        assert_eq!(error.code, Code::Format);
    }
}
