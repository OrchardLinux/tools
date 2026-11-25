//! The asset lane: what the installer fetches or builds, when it may start,
//! and whether Install can write yet. Names and states only; the app's
//! executors do the fetching.
//!
//! Triggers, as early as each asset's inputs allow:
//! - app start: the root image (one per root architecture; every current
//!   target is arm64) and palera1n for this host;
//! - board known (Found): the platform layer, the peripheral firmware, the
//!   chain payloads (Pongo, m1n1) and the kernel package;
//! - Continue on Found: the overlay for the iPadOS branch.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Asset {
    /// The distro's root, per root architecture.
    RootImage,
    /// Kernel, DTBs and modules for the board's platform.
    PlatformLayer,
    /// Peripheral firmware built from the board's recipe (byte ranges of the IPSW).
    Firmware,
    /// The pinned palera1n release for this host.
    Palera1n,
    /// Pongo and m1n1 for the board.
    Chain,
    /// The iPadOS overlay with the storage companion.
    Overlay,
    /// The root's kernel package for the board's layer, when one is published
    /// for exactly that layer.
    KernelPackage,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AssetState {
    #[default]
    NotStarted,
    Fetching,
    Ready,
    /// Failed for good: why, and the error itself.
    Failed(Cause, String),
}

/// Why an asset failed, as far as the user can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The network kept failing, for minutes: the connection may be down.
    Network,
    /// The installer can't use what it got, such as a firmware recipe it
    /// doesn't understand or a file that doesn't match its hash. The
    /// connection is fine; a newer installer may be needed.
    Data,
    /// Anything else, such as a release that doesn't cover the board yet.
    Other,
}

impl Cause {
    /// The failure dialog's title and text; `what` names the asset, such as
    /// "part of Omarchy".
    pub fn copy(self, what: &str) -> (&'static str, String) {
        match self {
            Cause::Network => (
                "A download didn't work",
                format!("The installer couldn't download {what}. Check the internet connection, then try again."),
            ),
            Cause::Data => (
                "The installer can't prepare this device",
                format!(
                    "The installer couldn't prepare {what}: it can't use the files it got for this device. \
                     A newer installer may be needed."
                ),
            ),
            Cause::Other => (
                "Something didn't work",
                format!("The installer couldn't prepare {what}. Try again; the details below say what went wrong."),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Assets {
    pub root_image: AssetState,
    pub platform_layer: AssetState,
    pub firmware: AssetState,
    pub palera1n: AssetState,
    pub chain: AssetState,
    pub overlay: AssetState,
    pub kernel_package: AssetState,
}

impl Assets {
    pub fn get(&self, asset: Asset) -> &AssetState {
        match asset {
            Asset::RootImage => &self.root_image,
            Asset::PlatformLayer => &self.platform_layer,
            Asset::Firmware => &self.firmware,
            Asset::Palera1n => &self.palera1n,
            Asset::Chain => &self.chain,
            Asset::Overlay => &self.overlay,
            Asset::KernelPackage => &self.kernel_package,
        }
    }

    pub fn set(&mut self, asset: Asset, state: AssetState) {
        let slot = match asset {
            Asset::RootImage => &mut self.root_image,
            Asset::PlatformLayer => &mut self.platform_layer,
            Asset::Firmware => &mut self.firmware,
            Asset::Palera1n => &mut self.palera1n,
            Asset::Chain => &mut self.chain,
            Asset::Overlay => &mut self.overlay,
            Asset::KernelPackage => &mut self.kernel_package,
        };
        *slot = state;
    }

    /// Marks the assets as fetching and returns the ones that were not yet
    /// started, so a trigger that fires twice starts nothing twice.
    pub fn start(&mut self, assets: &[Asset]) -> Vec<Asset> {
        let mut started = Vec::new();
        for &asset in assets {
            if *self.get(asset) == AssetState::NotStarted {
                self.set(asset, AssetState::Fetching);
                started.push(asset);
            }
        }
        started
    }

    /// What the install writes: the system and the board's files.
    pub fn ready_to_write(&self) -> bool {
        [&self.root_image, &self.platform_layer, &self.firmware, &self.kernel_package]
            .iter()
            .all(|s| **s == AssetState::Ready)
    }

    /// What booting iPadOS with the overlay needs: palera1n and the overlay.
    pub fn ready_to_boot_ipados(&self) -> bool {
        self.palera1n == AssetState::Ready && self.overlay == AssetState::Ready
    }

    /// The first failed asset, for an error the app can show with Try again.
    pub fn failed(&self) -> Option<(Asset, Cause, &str)> {
        ALL.iter().find_map(|&a| match self.get(a) {
            AssetState::Failed(cause, reason) => Some((a, *cause, reason.as_str())),
            _ => None,
        })
    }
}

pub const ALL: [Asset; 7] = [
    Asset::RootImage,
    Asset::PlatformLayer,
    Asset::Firmware,
    Asset::Palera1n,
    Asset::Chain,
    Asset::Overlay,
    Asset::KernelPackage,
];
pub const AT_START: [Asset; 2] = [Asset::RootImage, Asset::Palera1n];
pub const WITH_BOARD: [Asset; 4] = [Asset::PlatformLayer, Asset::Firmware, Asset::Chain, Asset::KernelPackage];

/// How a release file packages the executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    Dmg,
    TarGz,
    /// The release file is the executable itself.
    Bare,
}

/// A release file of a tool for one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin {
    pub os: &'static str,
    pub arch: &'static str,
    pub asset: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    pub package: Package,
    /// The executable's path inside the package. On macOS the DMG holds an
    /// HFS+ volume with an app bundle whose binary is the CLI (universal).
    pub member: Option<&'static str>,
    /// SHA-256 of the executable itself, checked after unpacking, where it
    /// has been recorded from the release file. The package hash always is.
    pub executable_sha256: Option<&'static str>,
}

pub const PALERA1N_RELEASE: &str = "https://github.com/palera1n/palera1n/releases/download/v3.0.0-beta.4";

/// palera1n v3.0.0-beta.4, from the release's published asset digests. macOS
/// has one universal build. Executable hashes were taken by unpacking each
/// build here (macOS through formats::udif + formats::hfs).
pub const PALERA1N_PINS: &[Pin] = &[
    Pin {
        os: "macos",
        arch: "aarch64",
        asset: "palera1n-macos-universal.dmg",
        size: 17_602_201,
        sha256: "f61de50eea582c2647c55602c70251e8b20b867436a1a299a53a946f52bda296",
        package: Package::Dmg,
        member: Some("palera1n.app/Contents/MacOS/palera1n"),
        executable_sha256: Some("b7e5d082e7bd5dbd03d47bc4ad7b1438a98d4cf4185c7f9599fa10df06f7020e"),
    },
    Pin {
        os: "macos",
        arch: "x86_64",
        asset: "palera1n-macos-universal.dmg",
        size: 17_602_201,
        sha256: "f61de50eea582c2647c55602c70251e8b20b867436a1a299a53a946f52bda296",
        package: Package::Dmg,
        member: Some("palera1n.app/Contents/MacOS/palera1n"),
        executable_sha256: Some("b7e5d082e7bd5dbd03d47bc4ad7b1438a98d4cf4185c7f9599fa10df06f7020e"),
    },
    Pin {
        os: "linux",
        arch: "aarch64",
        asset: "palera1n-linux-arm64.tar.gz",
        size: 5_285_880,
        sha256: "29f308fcfa5f2aa831b475fd9bba1318e4dc5de4ac4e2bc9516314b9232c56a0",
        package: Package::TarGz,
        member: Some("bin/palera1n"),
        executable_sha256: Some("edf1f4a98865c4326d71b3ed975cd050076082c0da4787bce4ea535c7c8d9b2b"),
    },
    Pin {
        os: "linux",
        arch: "x86_64",
        asset: "palera1n-linux-x86_64.tar.gz",
        size: 5_345_769,
        sha256: "eb636e5f84c49804a4d34f6d2a7be22210aaaaefeaf6a463345644e60e26b084",
        package: Package::TarGz,
        member: Some("bin/palera1n"),
        executable_sha256: Some("6aac0aee825cea96656d8d873944065a7b6ab89b5fa97582b8d0d5c23a14f1b9"),
    },
];

pub const PALERA1N_2_RELEASE: &str = "https://github.com/palera1n/palera1n/releases/download/v2.4";

/// palera1n v2.4, the stable release orchard's Python CLI pins (same sizes
/// and SHA-256): bare executables. Run instead of the beta only when a tester
/// turns the beta off (ORCHARD_PALERA1N_BETA=0); the overlay is still built
/// from the beta's binpack.
pub const PALERA1N_2_PINS: &[Pin] = &[
    Pin {
        os: "macos",
        arch: "aarch64",
        asset: "palera1n-macos-universal",
        size: 9_757_024,
        sha256: "dcf2f827e89a12215685c55f0462cd7d0cbc0551e3acd340bf0216c274c2e83e",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
    Pin {
        os: "macos",
        arch: "x86_64",
        asset: "palera1n-macos-universal",
        size: 9_757_024,
        sha256: "dcf2f827e89a12215685c55f0462cd7d0cbc0551e3acd340bf0216c274c2e83e",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
    Pin {
        os: "linux",
        arch: "aarch64",
        asset: "palera1n-linux-arm64",
        size: 5_059_168,
        sha256: "0ddcc3e703709f9cf9a85eb7c598f7500e2028fc4c60f3085eecb0b2bcbb138b",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
    Pin {
        os: "linux",
        arch: "x86_64",
        asset: "palera1n-linux-x86_64",
        size: 5_042_624,
        sha256: "ea531df933c0c8edab25ecb8f43faedd010177484482bb3744eaa4cf1cd48658",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
];

pub const CHECKRA1N_RELEASE: &str = "https://assets.checkra.in/downloads/preview/0.1337.3";

/// checkra1n 0.1337.3, the unpatched official build (palera1n embeds the
/// same version with patches of its own): bare executables, with the sizes
/// and SHA-256 orchard's Python CLI pins. Run for the exploit and Pongo
/// only when asked for (ORCHARD_CHECKRA1N=1).
pub const CHECKRA1N_PINS: &[Pin] = &[
    Pin {
        os: "macos",
        arch: "aarch64",
        asset: "checkra1n-macos",
        size: 1_161_528,
        sha256: "3bc1a0247dd9782b18de8945e3bb3b2dc740208b8c78f78634b80a7804a9f9be",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
    Pin {
        os: "macos",
        arch: "x86_64",
        asset: "checkra1n-macos",
        size: 1_161_528,
        sha256: "3bc1a0247dd9782b18de8945e3bb3b2dc740208b8c78f78634b80a7804a9f9be",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
    Pin {
        os: "linux",
        arch: "aarch64",
        asset: "checkra1n-linux-arm64",
        size: 550_664,
        sha256: "078053c11e9a301f0e71b87b6b854e6c8a69c87972325dd0c90deeeba1c3cb70",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
    Pin {
        os: "linux",
        arch: "x86_64",
        asset: "checkra1n-linux-x86_64",
        size: 554_232,
        sha256: "53154d87594cf67c56659f24547f1b5bb5336f84ea58c2d42f25a7b2df81a964",
        package: Package::Bare,
        member: None,
        executable_sha256: None,
    },
];

/// The checkra1n pin for a host.
pub fn checkra1n_pin(os: &str, arch: &str) -> Option<&'static Pin> {
    CHECKRA1N_PINS.iter().find(|p| p.os == os && p.arch == arch)
}

/// The v2.4 pin for a host.
pub fn palera1n_2_pin(os: &str, arch: &str) -> Option<&'static Pin> {
    PALERA1N_2_PINS.iter().find(|p| p.os == os && p.arch == arch)
}

/// The pin for a host, if palera1n publishes one.
pub fn palera1n_pin(os: &str, arch: &str) -> Option<&'static Pin> {
    PALERA1N_PINS.iter().find(|p| p.os == os && p.arch == arch)
}
