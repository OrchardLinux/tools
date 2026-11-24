//! This computer's time zone, for the installed system: the zone
//! /etc/localtime points to (macOS and Linux), as "Area/City". None when it
//! can't be read; the system then stays on UTC.
pub fn here() -> Option<String> {
    zone_of(&std::fs::read_link("/etc/localtime").ok()?.to_string_lossy())
}

/// "…/zoneinfo/Europe/Berlin" → "Europe/Berlin".
fn zone_of(target: &str) -> Option<String> {
    let zone = target.split("zoneinfo/").nth(1)?.trim_matches('/');
    let ok = !zone.is_empty()
        && !zone.split('/').any(|p| p.is_empty() || p == "." || p == "..")
        && zone.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b));
    ok.then(|| zone.to_owned())
}

#[cfg(test)]
mod tests {
    use super::zone_of;

    #[test]
    fn the_zone_comes_from_the_link_target() {
        assert_eq!(zone_of("/var/db/timezone/zoneinfo/Europe/Berlin").as_deref(), Some("Europe/Berlin"));
        assert_eq!(zone_of("../usr/share/zoneinfo/America/Argentina/Buenos_Aires").as_deref(), Some("America/Argentina/Buenos_Aires"));
        assert_eq!(zone_of("/usr/share/zoneinfo/../../etc/passwd"), None);
        assert_eq!(zone_of("/etc/localtime.local"), None);
    }
}
