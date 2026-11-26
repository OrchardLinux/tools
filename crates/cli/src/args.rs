use clap::{Args, Parser, Subcommand, ValueEnum};
use orchard_device::select::Selector;
use orchard_usb::Mode;

#[derive(Parser)]
#[command(
    name = "orchard2",
    version,
    about = "Boot Linux on iPhones and iPads over USB"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// Options every command takes.
#[derive(Args)]
pub struct Global {
    /// Print one JSON document instead of text
    #[arg(long, global = true)]
    pub json: bool,
    /// Orchard data directory, the one containing devices/; defaults to a
    /// copy of orchard's main branch
    #[arg(long, global = true, env = "ORCHARD_DATA")]
    pub data: Option<std::path::PathBuf>,
    /// Cache directory; defaults to the platform's cache directory
    #[arg(long, global = true, env = "ORCHARD_CACHE")]
    pub cache: Option<std::path::PathBuf>,
    #[command(flatten)]
    pub device: DeviceArgs,
}

/// How a running system is reached.
#[derive(Args, Debug, Clone)]
pub struct ReachArgs {
    /// An installed system's user (default: the one its install let this
    /// computer sign in as)
    #[arg(long, conflicts_with = "ram")]
    pub user: Option<String>,
    /// An SSH address, instead of the device on USB
    #[arg(long)]
    pub host: Option<String>,
    /// With --host: a RAM payload, reached as root
    #[arg(long, requires = "host")]
    pub ram: bool,
}

/// Which device to work on. Every command uses these same flags.
#[derive(Args, Default)]
pub struct DeviceArgs {
    /// Device ECID, in any hex spelling
    #[arg(long, global = true)]
    pub ecid: Option<String>,
    /// USB location, as `orchard2 devices` prints it
    #[arg(long, global = true)]
    pub location: Option<String>,
}

impl DeviceArgs {
    pub fn selector(&self, mode: Option<Mode>) -> Selector {
        Selector {
            ecid: self.ecid.clone(),
            location: self.location.clone(),
            mode,
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Install a system on your iPad or iPhone, from DFU mode (as the
    /// graphical installer does); --json prints one event per line
    Install(crate::commands::install::InstallArgs),
    /// Start a system installed on your iPad or iPhone, from DFU mode (as
    /// Orchard, the launcher, does)
    Launch(crate::commands::install::LaunchArgs),
    /// Build a project: DIRECTORY/init and optional DIRECTORY/rootfs into a
    /// RAM payload for every board of one platform; in a distribution
    /// checkout (orchard.toml [release]), its release
    Build(crate::commands::build::BuildArgs),
    /// Resolve what a distribution build takes from orchard finished
    /// (kernels, m1n1, Pongo, tools) into one JSON document
    Inputs(crate::commands::build::InputsArgs),
    /// Write the source keys of the components a nightly publishes
    /// (component-keys.json), from the data's files alone
    #[command(hide = true)]
    ComponentKeys {
        /// Where to write the document
        #[arg(long)]
        output: std::path::PathBuf,
        /// Platforms (default: every arm64 platform)
        platform: Vec<String>,
    },
    /// Start m1n1 from DFU mode, alone or with a Linux it boots in RAM
    Run(crate::commands::run::Args),
    /// List connected devices
    Devices(DevicesArgs),
    /// Wait until one device shows up in a mode
    Wait {
        /// The mode to wait for
        mode: ModeArg,
        #[command(flatten)]
        timeout: TimeoutArgs,
    },
    /// Open a shell on the running RAM payload or installed system
    Shell {
        #[command(flatten)]
        reach: ReachArgs,
    },
    /// Run a command on the running RAM payload or installed system:
    /// orchard2 exec -- CMD...
    Exec {
        #[command(flatten)]
        reach: ReachArgs,
        /// The command and its arguments, after `--`
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Copy a file of this computer to the running RAM payload or installed
    /// system, checked by SHA-256
    Push {
        /// The file here
        local: std::path::PathBuf,
        /// Where it goes on the device
        remote: String,
        #[command(flatten)]
        reach: ReachArgs,
        /// Over the USB serial console (a payload without SSH; up to 16 MiB)
        #[arg(long, conflicts_with_all = ["host", "ram"])]
        serial: bool,
    },
    /// Copy a file of the running RAM payload or installed system to this
    /// computer, checked by SHA-256
    Pull {
        /// The file on the device
        remote: String,
        /// Where it goes here
        local: std::path::PathBuf,
        #[command(flatten)]
        reach: ReachArgs,
        /// Over the USB serial console (a payload without SSH; up to 16 MiB)
        #[arg(long, conflicts_with_all = ["host", "ram"])]
        serial: bool,
    },
    /// Let this computer sign in over SSH to an installed system that was
    /// installed without it: signs in once on its USB serial console with
    /// the user's password
    SshAccess {
        /// The installed system's user (default: the one remembered, else asked)
        #[arg(long)]
        user: Option<String>,
    },
    /// Save Apple's device tree (ADT) of the device a RAM payload runs on,
    /// read over its USB serial console; for bringing up a board
    CaptureAdt {
        /// Where to save it; must not exist. The file names the unit: keep
        /// it private
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Keep one device locked while a command runs; orchard2 commands in it
    /// work on that device
    Session {
        /// Seconds all device work in the session may take together
        #[arg(long, value_parser = seconds)]
        timeout: Option<std::time::Duration>,
        /// The command to run, after `--`
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Prepare the device's boot chain: leave it in a receiver (Pongo, a
    /// patched iBoot or an autogo iBEC) that waits for a load
    Prep {
        #[command(flatten)]
        path: PathArgs,
        #[command(flatten)]
        firmware: FirmwareArgs,
        /// List the steps without touching the device
        #[arg(long)]
        plan: bool,
        /// Seconds all steps may take together
        #[arg(long, default_value = "300", value_parser = seconds)]
        timeout: std::time::Duration,
    },
    /// Put code on a device and start it, through the receiver it waits
    /// in or one a prep leaves it in
    Load {
        /// The file to load: a plain image for Pongo, m1n1's Mach-O for a
        /// patched iBoot, or Cascadia's loader; with --kernel, what is packed
        /// behind it
        file: std::path::PathBuf,
        #[command(flatten)]
        payload: PayloadArgs,
        #[command(flatten)]
        path: PathArgs,
        #[command(flatten)]
        firmware: FirmwareArgs,
        /// Also wait for the device to show up in this mode
        #[arg(long)]
        wait: Option<ModeArg>,
        /// List the steps without touching the device
        #[arg(long)]
        plan: bool,
        /// Seconds all steps may take together
        #[arg(long, default_value = "300", value_parser = seconds)]
        timeout: std::time::Duration,
    },
    /// Start a system installed on the device, through m1n1; a device
    /// that does not run m1n1 yet is taken there by its boot path
    Boot {
        /// The boot entry to start; without it, the first that can boot
        #[arg(long, conflicts_with = "list")]
        boot_entry: Option<String>,
        #[command(flatten)]
        path: PathArgs,
        #[command(flatten)]
        firmware: FirmwareArgs,
        /// Only list the boot entries
        #[arg(long)]
        list: bool,
        /// File name of the board's device tree; defaults to the board's,
        /// when the device has been recognised
        #[arg(long)]
        dtb_name: Option<String>,
        /// Do not show the system's splash picture while it loads
        #[arg(long)]
        no_splash: bool,
        /// Also wait for the device to show up in this mode
        #[arg(long, conflicts_with = "list")]
        wait: Option<ModeArg>,
        /// List the steps without touching the device
        #[arg(long)]
        plan: bool,
        /// Seconds all steps may take together
        #[arg(long, default_value = "300", value_parser = seconds)]
        timeout: std::time::Duration,
    },
    /// Look inside firmware archives (IPSWs)
    Fw(FwArgs),
    /// List the boards in the orchard data
    Boards,
    /// Show what the orchard data says about one board
    Board {
        /// Board name, as `boards` lists them
        name: String,
    },
    /// Show where board data comes from, or refresh it
    Data {
        #[command(subcommand)]
        command: Option<DataCommand>,
    },
    /// Show what this computer has of what orchard2 uses, and what each is
    /// needed for; changes nothing
    Doctor,
    /// Get one of the things doctor lists as missing; without a name, show
    /// them
    Setup {
        /// What to get, as doctor names it: usb, podman, git, python
        names: Vec<String>,
    },
    /// Show the cache, or delete from it: downloaded files and build
    /// components
    Cache {
        /// Default: ls
        #[command(subcommand)]
        command: Option<CacheCommand>,
    },
    /// Talk to a device in DFU mode
    Dfu {
        #[command(flatten)]
        timeout: TimeoutArgs,
        #[command(subcommand)]
        command: DfuCommand,
    },
    /// Talk to a device in recovery mode (iBoot)
    Recovery {
        #[command(flatten)]
        timeout: TimeoutArgs,
        #[command(subcommand)]
        command: RecoveryCommand,
    },
    /// Talk to a device running PongoOS
    Pongo {
        #[command(flatten)]
        timeout: TimeoutArgs,
        #[command(subcommand)]
        command: PongoCommand,
    },
    /// Talk to the m1n1 proxy
    M1n1 {
        #[command(flatten)]
        timeout: TimeoutArgs,
        #[command(subcommand)]
        command: M1n1Command,
    },
}

/// Which methods take the device to the receiver. They are shown by
/// `--plan`.
#[derive(Args)]
pub struct PathArgs {
    /// The state the device is in, where USB does not show it, such as
    /// kdfu, patched-recovery or autogo
    #[arg(long, value_name = "STATE", value_parser = state)]
    pub from: Option<orchard_device::methods::State>,
    /// The entry tool to start with, where several fit
    #[arg(long, value_name = "NAME")]
    pub entry: Option<String>,
    /// The boot chain to prepare with, where several fit
    #[arg(long, value_name = "NAME")]
    pub chain: Option<String>,
    /// The board, where the device does not report it; with --plan and
    /// --from, the plan is for that board and state, and no device is
    /// looked at
    #[arg(long, value_name = "NAME")]
    pub board: Option<String>,
}

/// What a boot chain's firmware is built with, where a chain is prepared.
#[derive(Args)]
pub struct FirmwareArgs {
    /// Firmware key file (also ORCHARD_FIRMWARE_KEYS)
    #[arg(long)]
    pub keys: Option<std::path::PathBuf>,
    /// IM4M manifest; defaults to the bundled chip manifest
    #[arg(long)]
    pub im4m: Option<std::path::PathBuf>,
}

fn state(text: &str) -> Result<orchard_device::methods::State, String> {
    use orchard_device::methods::State;
    State::parse(text).ok_or_else(|| {
        let names: Vec<&str> = State::ALL.into_iter().map(State::as_str).collect();
        format!("not a state; the states are {}", names.join(", "))
    })
}

/// What m1n1 or Cascadia's loader is to boot, packed behind it.
#[derive(Args)]
pub struct PayloadArgs {
    /// Kernel image
    #[arg(long, requires = "dtb")]
    pub kernel: Option<std::path::PathBuf>,
    /// Device tree; repeat for several, which keep their order
    #[arg(long, requires = "kernel")]
    pub dtb: Vec<std::path::PathBuf>,
    /// Initramfs archive, compressed or not
    #[arg(long, requires = "kernel")]
    pub initramfs: Option<std::path::PathBuf>,
    /// Kernel command line
    #[arg(long, requires = "kernel")]
    pub bootargs: Option<String>,
    /// Boot logo in m1n1's own format
    #[arg(long, requires = "kernel")]
    pub logo: Option<std::path::PathBuf>,
}

/// How long one device action may take.
#[derive(Args)]
pub struct TimeoutArgs {
    /// Seconds the action may take
    #[arg(long, global = true, default_value = "30", value_parser = seconds)]
    pub timeout: std::time::Duration,
}

fn seconds(text: &str) -> Result<std::time::Duration, String> {
    let seconds: f64 = text.parse().map_err(|_| "not a number of seconds")?;
    std::time::Duration::try_from_secs_f64(seconds).map_err(|_| "not a time span".into())
}

/// A number in decimal, or in hex with `0x`.
fn number(text: &str) -> Result<u64, String> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| "not a decimal or 0x hex number".into())
}

#[derive(Subcommand)]
pub enum DfuCommand {
    /// Read the DFU status and state
    Status,
    /// Clear an error status; the device returns to idle
    ClearStatus,
    /// Abandon a transfer in progress; the device returns to idle
    Abort,
    /// Send an image and make the boot ROM run it
    Send {
        /// The image file
        file: std::path::PathBuf,
    },
    /// Reset the device's USB port
    Reset,
}

#[derive(Subcommand)]
pub enum RecoveryCommand {
    /// Upload a file into iBoot's load buffer
    Send {
        /// The file to upload
        file: std::path::PathBuf,
    },
    /// Run one iBoot command
    Command {
        /// The command line, as one argument
        command: String,
    },
    /// Read one iBoot environment variable
    Getenv {
        /// Variable name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum PongoCommand {
    /// Read the shell's new output and whether it is busy
    Output,
    /// Upload a file into Pongo's load buffer
    Send {
        /// The file to upload
        file: std::path::PathBuf,
    },
    /// Send one command line to the shell
    Command {
        /// The command line, as one argument
        command: String,
    },
}

#[derive(Subcommand)]
pub enum M1n1Command {
    /// Pack m1n1 and what it is to boot into one image file; needs no device
    Pack {
        /// m1n1 itself
        file: std::path::PathBuf,
        #[command(flatten)]
        payload: PayloadArgs,
        /// Where to write the image
        #[arg(long, short)]
        out: std::path::PathBuf,
    },
    /// Check that the proxy answers
    Ping,
    /// Run one proxy operation and print its result
    Call {
        /// Operation number
        #[arg(value_parser = number)]
        operation: u64,
        /// Up to six arguments
        #[arg(value_parser = number, num_args = 0..=6)]
        args: Vec<u64>,
    },
    /// Read device memory
    Read {
        /// Start address
        #[arg(value_parser = number)]
        address: u64,
        /// Number of bytes
        #[arg(value_parser = number)]
        size: u64,
    },
    /// Write a file into device memory
    Write {
        /// Start address
        #[arg(value_parser = number)]
        address: u64,
        /// The file to write
        file: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
pub enum DataCommand {
    /// Fetch orchard's main branch now, and which m1n1 and Pongo are current
    Update,
}

#[derive(Subcommand)]
pub enum CacheCommand {
    /// List cached files and build components
    Ls,
    /// Delete all cached files
    Clear,
    /// Delete build components
    Rm {
        /// COMPONENT (all of its entries) or COMPONENT/KEY-PREFIX, such as
        /// kernel-j71b/0bd1204e
        #[arg(required = true)]
        entries: Vec<String>,
    },
    /// Keep only the most recently used build components of each kind
    Prune {
        /// Entries per component to keep
        #[arg(long, default_value = "5")]
        keep: usize,
    },
}

#[derive(Args)]
pub struct FwArgs {
    #[command(subcommand)]
    pub command: Fw,
}

#[derive(Subcommand)]
pub enum Fw {
    /// Show the version, the boards a firmware covers, and a board's recipe
    Info {
        #[command(flatten)]
        source: SourceArgs,
    },
    /// List archive members, or a directory inside a disk image with --image
    Ls {
        #[command(flatten)]
        source: SourceArgs,
        /// Directory inside the disk image
        #[arg(default_value = "/")]
        path: String,
        #[command(flatten)]
        image: ImageArgs,
    },
    /// Build a board's boot or peripheral firmware into a directory
    Build {
        /// Board name from the orchard data
        board: String,
        #[arg(long = "type", value_enum, default_value = "boot")]
        kind: KindArg,
        /// Boot chain; defaults to the board's sole chain
        #[arg(long)]
        chain: Option<String>,
        /// Firmware key file (also ORCHARD_FIRMWARE_KEYS)
        #[arg(long)]
        keys: Option<std::path::PathBuf>,
        /// IM4M manifest; defaults to the bundled chip manifest
        #[arg(long)]
        im4m: Option<std::path::PathBuf>,
        /// Directory to write into; files go below it at their destinations
        #[arg(long)]
        out: std::path::PathBuf,
        /// Acquire missing boot keys from the selected DFU device (may run gaster)
        #[arg(long)]
        device_keys: bool,
        /// Build what this version can, and list the outputs it cannot make
        #[arg(long)]
        skip_unsupported: bool,
    },
    /// Print a recipe input, an archive member, or a file inside a disk image with --image
    Cat {
        #[command(flatten)]
        source: SourceArgs,
        /// Recipe input name (with a board), member name, or file path inside the disk image
        path: String,
        #[command(flatten)]
        image: ImageArgs,
    },
}

/// Which firmware to open.
#[derive(Args)]
pub struct SourceArgs {
    /// A board name from the orchard data, an https:// URL, or a local file
    pub firmware: String,
    /// With a board name: the boot recipe or the peripheral firmware recipe
    #[arg(long = "type", value_enum, default_value = "boot")]
    pub kind: KindArg,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum KindArg {
    Boot,
    Peripheral,
}

impl From<KindArg> for orchard_boards::catalog::Kind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Boot => Self::Boot,
            KindArg::Peripheral => Self::Peripheral,
        }
    }
}

/// Which disk image of a firmware to look into.
#[derive(Args)]
pub struct ImageArgs {
    /// Manifest component holding the disk image, such as OS
    #[arg(long)]
    pub image: Option<String>,
    /// Board, as `fw info` lists them; needed when the firmware covers several
    #[arg(long, requires = "image")]
    pub board: Option<String>,
    /// Volume name; needed when the disk image has several
    #[arg(long, requires = "image")]
    pub volume: Option<String>,
}

#[derive(Args)]
pub struct DevicesArgs {
    /// Only devices in this mode
    #[arg(long)]
    pub mode: Option<ModeArg>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ModeArg {
    Dfu,
    Recovery,
    Pongo,
    M1n1,
    Linux,
}

impl From<ModeArg> for Mode {
    fn from(m: ModeArg) -> Mode {
        match m {
            ModeArg::Dfu => Mode::Dfu,
            ModeArg::Recovery => Mode::Recovery,
            ModeArg::Pongo => Mode::Pongo,
            ModeArg::M1n1 => Mode::M1n1,
            ModeArg::Linux => Mode::Linux,
        }
    }
}

impl Global {
    /// The board data directory: `--data`, or the copy of orchard's main
    /// branch, fetched or refreshed as needed.
    pub fn data_dir(&self) -> orchard_cache::Result<std::path::PathBuf> {
        let store = orchard_cache::store::Store::open(self.cache.as_deref())?;
        Ok(orchard_boards::data::board_data::resolve(self.data.as_deref(), &store)?.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_chosen_for_a_run_as_for_a_plan() {
        for args in [
            &["orchard2", "boot", "--entry", "palera1n"][..],
            &["orchard2", "boot", "--entry", "palera1n", "--plan"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
    }
}

#[cfg(test)]
mod documented {
    use super::*;
    use clap::CommandFactory;

    /// Every command and every option is in `docs/`, which the published
    /// documentation is made from.
    #[test]
    fn every_command_and_option_is_in_the_docs() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
        let mut docs = String::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "md") {
                docs.push_str(&std::fs::read_to_string(path).unwrap());
            }
        }
        // A name counts where it stands by itself: `--keep` is not `--keep-going`.
        let stands = |name: &str| {
            docs.match_indices(name).any(|(at, _)| {
                let after = docs[at + name.len()..].chars().next();
                let before = docs[..at].chars().next_back();
                !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-') && !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
            })
        };
        let mut missing = Vec::new();
        let mut open = vec![(String::new(), Cli::command())];
        while let Some((path, command)) = open.pop() {
            for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
                if let Some(long) = arg.get_long().filter(|long| !matches!(*long, "help" | "version")) {
                    if !stands(&format!("--{long}")) {
                        missing.push(format!("{path} --{long}"));
                    }
                }
            }
            for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set() && sub.get_name() != "help") {
                let name = format!("{path} {}", sub.get_name()).trim().to_owned();
                if !stands(&name) {
                    missing.push(name.clone());
                }
                open.push((name, sub.clone()));
            }
        }
        missing.sort();
        assert!(missing.is_empty(), "not in docs/: {missing:#?}");
    }
}
