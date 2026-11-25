//! Peripheral firmware: the files Linux needs on the device (Wi-Fi,
//! Bluetooth, touch, …), made from a board's firmware recipe. Install and
//! RAM boot both use this.
use crate::catalog::{self, Check, Output, Recipe};
use crate::data::firmware::Inputs;
use crate::firmware_build::{write, Written};
use orchard_cache::store::sha256;
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::kernelcache::{self, Kernelcache};
use orchard_formats::z2fw::{self, Gen1, Profile};
use orchard_formats::{aop, bluetool, im4p, trx};
use std::collections::HashMap;
use std::path::{Component, Path};

/// Inputs and results larger than this are refused.
const MAX_FILE: u64 = 256 << 20;
/// The Wi-Fi address firmware carries in place of a unit's own. The platform
/// layer replaces it at boot.
const PLACEHOLDER_MAC: &str = "02:00:00:00:00:00";

/// A step this version knows, with its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Copy,
    BluetoolHcd,
    TrxAddNvram {
        nvram: String,
    },
    Im4pPayload,
    /// SmartIO firmware from the kext with this bundle id.
    Smartio {
        kext: String,
    },
    Vxd,
    AgxG9p,
    /// A9 GPU firmware, the one C0 image the driver knows.
    AgxG5p,
    /// A8 GPU firmware and its border colour table; the recipe's hash picks
    /// each from the candidates in the kernel.
    AgxG4pLdr,
    AgxG4pBorder,
    AopImage {
        coprocessor: String,
    },
    /// Touch firmware from an mtprops profile or MTFW personality; see `z2fw`.
    Touch {
        profile: String,
        kind: Touch,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Touch {
    Packet {
        packet: String,
        load_address: u64,
        hbpp_version: u64,
    },
    Z2Packet {
        calibration_address: u64,
        calibration_size: u64,
    },
    Packets {
        packet: String,
    },
    Gen1 {
        packet: String,
        values: [u64; 5],
    },
    Mtfw {
        otp_preflight: bool,
    },
    Hidspi,
}

/// What a recipe will do, worked out before anything is fetched.
#[derive(Debug)]
pub struct Plan<'a> {
    build: Vec<(&'a Output, Vec<Step>)>,
    /// Outputs not yet checked against real inputs; never built.
    pub pending: Vec<&'a str>,
    /// Outputs needing a step this version lacks: destination and step name.
    pub unsupported: Vec<(&'a str, &'a str)>,
}

impl<'a> Plan<'a> {
    /// Checks every output's steps and parameters.
    pub fn new(recipe: &'a Recipe) -> Result<Plan<'a>> {
        let mut plan = Plan {
            build: Vec::new(),
            pending: Vec::new(),
            unsupported: Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        for output in &recipe.outputs {
            let dest = output.destination.as_str();
            if !seen.insert(dest) || !relative(dest) {
                return Err(Error::new(
                    Code::Format,
                    "destination is repeated or not a plain relative path",
                )
                .context(dest));
            }
            let first_reads_input = output.steps.first().is_some_and(|s| s.input.is_some());
            if !first_reads_input || output.steps.iter().skip(1).any(|s| s.input.is_some()) {
                return Err(
                    Error::new(Code::Format, "only the first step reads an input").context(dest),
                );
            }
            if let Check::Pending(_) = output.check {
                plan.pending.push(dest);
                continue;
            }
            let mut steps = Vec::new();
            for raw in &output.steps {
                match parse(raw, recipe).context(dest)? {
                    Some(step) => steps.push(step),
                    None => {
                        plan.unsupported.push((dest, raw.name.as_str()));
                        break;
                    }
                }
            }
            if steps.len() == output.steps.len() {
                plan.build.push((output, steps));
            }
        }
        Ok(plan)
    }

    /// Makes every buildable output into `root`, checking each against its
    /// hash, and returns what was written.
    pub fn build(&self, inputs: &mut Inputs, root: &Path) -> Result<Vec<Written>> {
        let mut written = Vec::new();
        let mut kernels = HashMap::new();
        for (output, steps) in &self.build {
            let dest = &output.destination;
            let bytes = run(output, steps, inputs, &mut kernels).context(dest)?;
            let actual = sha256(&bytes);
            if let Check::Sha256(expected) = &output.check {
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(Error::new(
                        Code::Format,
                        format!("SHA-256 is {actual}, the recipe expects {expected}"),
                    )
                    .context(dest));
                }
            }
            written.push(write(root, dest, &bytes)?);
        }
        Ok(written)
    }
}

/// Decoded kernels by input name, so several outputs share one decode.
type Kernels = HashMap<String, Vec<u8>>;

fn run(
    output: &Output,
    steps: &[Step],
    inputs: &mut Inputs,
    kernels: &mut Kernels,
) -> Result<Vec<u8>> {
    let first = output.steps[0].input.as_deref().unwrap_or_default();
    let mut data = inputs.get(first, MAX_FILE)?.bytes;
    for step in steps {
        data = match step {
            Step::Smartio { kext } => {
                kernelcache::smartio(&Kernelcache::new(kernel(kernels, first, &data)?)?, kext)?
                    .to_vec()
            }
            Step::Vxd => {
                kernelcache::vxd(&Kernelcache::new(kernel(kernels, first, &data)?)?)?.to_vec()
            }
            Step::AgxG9p => {
                kernelcache::agx_g9p(&Kernelcache::new(kernel(kernels, first, &data)?)?)?.to_vec()
            }
            Step::AgxG5p => {
                let image =
                    kernelcache::agx_g5p(&Kernelcache::new(kernel(kernels, first, &data)?)?)?;
                if !sha256(image).eq_ignore_ascii_case(kernelcache::AGX_G5P_SHA256) {
                    return Err(Error::new(
                        Code::Format,
                        "the kernelcache's G5P firmware is not the C0 image the driver knows",
                    ));
                }
                image.to_vec()
            }
            Step::AgxG4pLdr => pinned(
                output,
                kernelcache::agx_g4p_ldrs(kernel(kernels, first, &data)?),
                "A8 GPU firmware",
            )?,
            Step::AgxG4pBorder => pinned(
                output,
                kernelcache::agx_g4p_borders(kernel(kernels, first, &data)?),
                "A8 GPU border colour table",
            )?,
            Step::AopImage { coprocessor } => aop::image(&data, coprocessor)?,
            Step::Touch { profile, kind } => touch(&data, profile, kind)?,
            Step::Copy => data,
            // BlueTool holds several streams; the recipe's hash picks one.
            Step::BluetoolHcd => {
                pinned(output, bluetool::hcd_streams(&data), "Bluetooth firmware")?
            }
            Step::TrxAddNvram { nvram } => {
                trx::add_nvram(&data, &inputs.get(nvram, MAX_FILE)?.bytes, PLACEHOLDER_MAC)?
            }
            Step::Im4pPayload => {
                let payload = im4p::payload(&data)?;
                if payload.starts_with(b"bvx") {
                    return Err(Error::new(Code::Format, "IM4P payload is compressed"));
                }
                payload.to_vec()
            }
        };
    }
    Ok(data)
}

fn touch(data: &[u8], profile: &str, kind: &Touch) -> Result<Vec<u8>> {
    let mtprops = || Profile::from_mtprops(data, profile);
    Ok(match kind {
        Touch::Packet {
            packet,
            load_address,
            hbpp_version,
        } => z2fw::from_packet(&mtprops()?, packet, *load_address, *hbpp_version)?,
        Touch::Z2Packet {
            calibration_address,
            calibration_size,
        } => z2fw::from_z2_packet(&mtprops()?, *calibration_address, *calibration_size)?,
        Touch::Packets { packet } => z2fw::from_packets(&mtprops()?, packet)?,
        Touch::Gen1 {
            packet,
            values: [touch_maximum, prox_maximum, fll_mval, ref_clk_div, clk32_clock_enable],
        } => {
            let board = Gen1 {
                touch_maximum: *touch_maximum,
                prox_maximum: *prox_maximum,
                fll_mval: *fll_mval,
                ref_clk_div: *ref_clk_div,
                clk32_clock_enable: *clk32_clock_enable,
            };
            z2fw::from_gen1(&mtprops()?, packet, &board)?
        }
        Touch::Mtfw { otp_preflight } => z2fw::from_mtfw(data, profile, *otp_preflight)?,
        Touch::Hidspi => z2fw::hidspi_from_mtfw(data, profile)?,
    })
}

/// The candidate with the output's pinned hash.
fn pinned(output: &Output, candidates: Vec<&[u8]>, what: &str) -> Result<Vec<u8>> {
    let Check::Sha256(expected) = &output.check else {
        unreachable!("pending outputs are not built")
    };
    candidates
        .into_iter()
        .find(|c| sha256(c).eq_ignore_ascii_case(expected))
        .map(<[u8]>::to_vec)
        .ok_or_else(|| Error::new(Code::Format, format!("no {what} with the recipe's hash")))
}

/// The decoded kernel for input `name`. A kernelcache input is the decoded
/// Mach-O already (that is what its hash covers); an IM4P, such as a recipe's
/// plain archive member, is decoded the first time.
fn kernel<'k>(kernels: &'k mut Kernels, name: &str, data: &[u8]) -> Result<&'k [u8]> {
    if !kernels.contains_key(name) {
        let decoded = if data.starts_with(&MH_MAGIC_64) || data.starts_with(&FAT_MAGIC) {
            orchard_formats::macho::arm64(data)?.to_vec()
        } else {
            kernelcache::decode(data)?
        };
        kernels.insert(name.to_owned(), decoded);
    }
    Ok(&kernels[name])
}

/// A 64-bit Mach-O, and a universal binary, as their files start.
const MH_MAGIC_64: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];
const FAT_MAGIC: [u8; 4] = [0xca, 0xfe, 0xba, 0xbe];

/// The step for a recipe step, `None` if this version does not know it.
fn parse(raw: &catalog::Step, recipe: &Recipe) -> Result<Option<Step>> {
    let string = |name: &str| -> Result<String> {
        raw.params
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::new(
                    Code::Format,
                    format!("step {} needs a text parameter {name}", raw.name),
                )
            })
    };
    let int = |name: &str| -> Result<u64> {
        raw.params
            .get(name)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                Error::new(
                    Code::Format,
                    format!("step {} needs an integer parameter {name}", raw.name),
                )
            })
    };
    let flag = |name: &str| -> Result<bool> {
        raw.params
            .get(name)
            .and_then(|v| v.as_bool())
            .ok_or_else(|| {
                Error::new(
                    Code::Format,
                    format!("step {} needs a true/false parameter {name}", raw.name),
                )
            })
    };
    let touch = |kind: Touch| -> Result<Step> {
        Ok(Step::Touch {
            profile: string("profile")?,
            kind,
        })
    };
    let (step, expected): (Step, &[&str]) = match raw.name.as_str() {
        "z2fw-from-mtprops@1" => (
            touch(Touch::Packet {
                packet: string("packet")?,
                load_address: int("load_address")?,
                hbpp_version: int("hbpp_version")?,
            })?,
            &["hbpp_version", "load_address", "packet", "profile"],
        ),
        "z2fw-from-z2-packet@1" => (
            touch(Touch::Z2Packet {
                calibration_address: int("calibration_address")?,
                calibration_size: int("calibration_size")?,
            })?,
            &["calibration_address", "calibration_size", "profile"],
        ),
        "z2fw-from-mtprops@2" => (
            touch(Touch::Packets {
                packet: string("packet")?,
            })?,
            &["packet", "profile"],
        ),
        "z2fw-from-mtprops@3" => {
            let values = [
                int("touch_maximum")?,
                int("prox_maximum")?,
                int("fll_mval")?,
                int("ref_clk_div")?,
                int("clk32_clock_enable")?,
            ];
            (
                touch(Touch::Gen1 {
                    packet: string("packet")?,
                    values,
                })?,
                &[
                    "clk32_clock_enable",
                    "fll_mval",
                    "packet",
                    "profile",
                    "prox_maximum",
                    "ref_clk_div",
                    "touch_maximum",
                ],
            )
        }
        "z2fw-from-mtfw@1" => (
            touch(Touch::Mtfw {
                otp_preflight: flag("otp_preflight")?,
            })?,
            &["otp_preflight", "profile"],
        ),
        "hidspi-from-mtfw@1" => (touch(Touch::Hidspi)?, &["profile"]),
        "copy@1" => (Step::Copy, &[]),
        "bluetool-hcd@1" => (Step::BluetoolHcd, &[]),
        "im4p-payload@1" => (Step::Im4pPayload, &[]),
        "smartio-from-kernelcache@1" => (
            Step::Smartio {
                kext: kernelcache::SMARTIO_KEXTS[0].to_owned(),
            },
            &[],
        ),
        "smartio-from-kernelcache@2" => {
            let kext = string("kext")?;
            if !kernelcache::SMARTIO_KEXTS.contains(&kext.as_str()) {
                return Err(Error::new(
                    Code::Format,
                    format!("unknown SmartIO kext {kext}"),
                ));
            }
            (Step::Smartio { kext }, &["kext"])
        }
        "vxd-firmware-from-kernelcache@1" => (Step::Vxd, &[]),
        "agx-g9p-firmware-from-kernelcache@1" => (Step::AgxG9p, &[]),
        "agx-g5p-ldr-from-kernelcache@1" => (Step::AgxG5p, &[]),
        "agx-g4p-ldr-from-kernelcache@1" => (Step::AgxG4pLdr, &[]),
        "agx-g4p-border-from-kernelcache@1" => (Step::AgxG4pBorder, &[]),
        "aop-image@1" => (
            Step::AopImage {
                coprocessor: string("coprocessor")?,
            },
            &["coprocessor"],
        ),
        "trx-add-nvram@1" => {
            if string("placeholder_mac")? != PLACEHOLDER_MAC {
                return Err(Error::new(
                    Code::Format,
                    format!("Wi-Fi firmware must carry the address {PLACEHOLDER_MAC}"),
                ));
            }
            (
                Step::TrxAddNvram {
                    nvram: string("nvram")?,
                },
                &["nvram", "placeholder_mac"],
            )
        }
        _ => return Ok(None),
    };
    let mut names: Vec<_> = raw.params.keys().map(String::as_str).collect();
    names.sort_unstable();
    if names != expected {
        return Err(Error::new(
            Code::Format,
            format!(
                "step {} has parameters {names:?}, expected {expected:?}",
                raw.name
            ),
        ));
    }
    let mut needed = raw.input.iter().chain(match &step {
        Step::TrxAddNvram { nvram } => Some(nvram),
        _ => None,
    });
    if let Some(missing) = needed.find(|n| !recipe.inputs.contains_key(*n)) {
        return Err(Error::new(
            Code::Format,
            format!("step {} reads unknown input {missing}", raw.name),
        ));
    }
    Ok(Some(step))
}

fn relative(path: &str) -> bool {
    let path = Path::new(path);
    path.components().all(|c| matches!(c, Component::Normal(_)))
        && path.components().next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Board, Catalog, Input, Ipsw, Location};
    use orchard_cache::store::{Key, Store};
    use std::collections::BTreeMap;

    fn step(name: &str, input: Option<&str>, params: &[(&str, &str)]) -> catalog::Step {
        catalog::Step {
            name: name.into(),
            input: input.map(Into::into),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::from(*v)))
                .collect(),
        }
    }

    fn output(dest: &str, data: &[u8], steps: Vec<catalog::Step>) -> Output {
        Output {
            destination: dest.into(),
            check: Check::Sha256(sha256(data)),
            steps,
        }
    }

    fn recipe(inputs: &[(&str, &[u8])], outputs: Vec<Output>) -> Recipe {
        Recipe {
            ipsw: Ipsw {
                version: "1".into(),
                build: "1A1".into(),
                url: "https://example.com/x.ipsw".into(),
                size: 1,
                sha256: "00".into(),
            },
            inputs: inputs
                .iter()
                .map(|(name, data)| {
                    (
                        name.to_string(),
                        Input {
                            location: Location::Member(name.to_string()),
                            sha256: Some(sha256(data)),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            outputs,
        }
    }

    /// A board from a throwaway catalog; only its numbers matter here.
    fn board(dir: &Path) -> Board {
        let devices = dir.join("devices/x1");
        std::fs::create_dir_all(&devices).unwrap();
        std::fs::write(
            devices.join("device.toml"),
            "[device]\nname = \"X1\"\n[match.dfu]\nchip = 1\nboard = 2\n",
        )
        .unwrap();
        Catalog::open(dir).unwrap().board("x1").unwrap()
    }

    #[test]
    fn gpu_firmware_step_is_supported_without_parameters() {
        let recipe = recipe(
            &[("kernelcache", b"kernel")],
            vec![output(
                "lib/firmware/apple/gfx-t8010.bin",
                b"gpu",
                vec![step(
                    "agx-g9p-firmware-from-kernelcache@1",
                    Some("kernelcache"),
                    &[],
                )],
            )],
        );
        let plan = Plan::new(&recipe).unwrap();
        assert!(plan.unsupported.is_empty());
        assert_eq!(plan.build[0].1, [Step::AgxG9p]);
    }

    #[test]
    fn a8_and_a9_gpu_firmware_steps_are_supported_without_parameters() {
        let steps = [
            ("agx-g5p-ldr-from-kernelcache@1", Step::AgxG5p),
            ("agx-g4p-ldr-from-kernelcache@1", Step::AgxG4pLdr),
            ("agx-g4p-border-from-kernelcache@1", Step::AgxG4pBorder),
        ];
        let outputs = steps
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                output(
                    &format!("lib/firmware/apple/{i}.bin"),
                    b"gpu",
                    vec![step(name, Some("kernelcache"), &[])],
                )
            })
            .collect();
        let recipe = recipe(&[("kernelcache", b"kernel")], outputs);
        let plan = Plan::new(&recipe).unwrap();
        assert!(plan.unsupported.is_empty());
        let built: Vec<_> = plan.build.iter().map(|(_, s)| s[0].clone()).collect();
        assert_eq!(built, steps.map(|(_, s)| s));
        let extra = step(
            "agx-g4p-ldr-from-kernelcache@1",
            Some("kernelcache"),
            &[("x", "1")],
        );
        let recipe = self::recipe(
            &[("kernelcache", b"kernel")],
            vec![output("lib/a", b"a", vec![extra])],
        );
        assert_eq!(Plan::new(&recipe).unwrap_err().code, Code::Format);
    }

    /// Every board of an orchard checkout, where `ORCHARD_TEST_DATA` names
    /// one: its peripheral recipe parses, and every GPU firmware output has
    /// steps this version runs.
    #[test]
    fn every_board_of_an_orchard_checkout_gets_its_gpu_firmware() {
        let Ok(dir) = std::env::var("ORCHARD_TEST_DATA") else {
            eprintln!("ORCHARD_TEST_DATA is not set; no orchard checkout to read");
            return;
        };
        let catalog = Catalog::open(Path::new(&dir)).unwrap();
        let mut failed = Vec::new();
        for name in catalog.boards().unwrap() {
            let board = catalog.board(&name).unwrap();
            let recipe = match board.recipe(crate::catalog::Kind::Peripheral) {
                Ok(recipe) => recipe,
                Err(e) if e.code == Code::NotFound => continue,
                Err(e) => {
                    failed.push(format!("{name}: {e}"));
                    continue;
                }
            };
            match Plan::new(&recipe) {
                Ok(plan) => failed.extend(
                    plan.unsupported
                        .iter()
                        .filter(|(dest, _)| dest.starts_with("lib/firmware/apple/gfx-"))
                        .map(|(dest, step)| format!("{name}: {dest} needs {step}")),
                ),
                Err(e) => failed.push(format!("{name}: {e}")),
            }
        }
        assert!(failed.is_empty(), "\n{}", failed.join("\n"));
    }

    #[test]
    fn the_output_hash_picks_the_candidate() {
        let out = output("lib/b", b"two", vec![]);
        let candidates = || vec![&b"one"[..], &b"two"[..]];
        assert_eq!(pinned(&out, candidates(), "x").unwrap(), b"two");
        let other = output("lib/b", b"three", vec![]);
        let e = pinned(&other, candidates(), "A8 GPU firmware").unwrap_err();
        assert_eq!(e.message, "no A8 GPU firmware with the recipe's hash");
    }

    #[test]
    fn builds_from_cached_inputs_and_checks_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
        let wifi = b"wifi firmware".as_slice();
        store.put(&Key::Sha256(sha256(wifi)), wifi).unwrap();
        let recipe = recipe(
            &[("wifi.bin", wifi)],
            vec![
                output(
                    "lib/firmware/brcm/wifi.bin",
                    wifi,
                    vec![step("copy@1", Some("wifi.bin"), &[])],
                ),
                Output {
                    destination: "lib/firmware/later.bin".into(),
                    check: Check::Pending("not measured".into()),
                    steps: vec![step("copy@1", Some("wifi.bin"), &[])],
                },
                output(
                    "lib/firmware/touch.bin",
                    b"?",
                    vec![step("z2fw-from-mtprops@9", Some("wifi.bin"), &[])],
                ),
            ],
        );
        let plan = Plan::new(&recipe).unwrap();
        assert_eq!(plan.pending, ["lib/firmware/later.bin"]);
        assert_eq!(
            plan.unsupported,
            [("lib/firmware/touch.bin", "z2fw-from-mtprops@9")]
        );
        let board = board(dir.path());
        let root = dir.path().join("out");
        let written = plan
            .build(&mut Inputs::new(&store, &board, &recipe), &root)
            .unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(
            std::fs::read(root.join("lib/firmware/brcm/wifi.bin")).unwrap(),
            wifi
        );
    }

    #[test]
    fn wrong_output_hash_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
        store.put(&Key::Sha256(sha256(b"a")), b"a").unwrap();
        let recipe = recipe(
            &[("a", b"a")],
            vec![output(
                "lib/a",
                b"not a",
                vec![step("copy@1", Some("a"), &[])],
            )],
        );
        let board = board(dir.path());
        let e = Plan::new(&recipe)
            .unwrap()
            .build(&mut Inputs::new(&store, &board, &recipe), dir.path())
            .unwrap_err();
        assert!(e.message.starts_with("lib/a: SHA-256 is"));
    }

    #[test]
    fn recipe_mistakes_are_caught_before_fetching() {
        let bad = |outputs| {
            Plan::new(&recipe(&[("a", b"a")], outputs))
                .unwrap_err()
                .code
        };
        let copy = || vec![step("copy@1", Some("a"), &[])];
        assert_eq!(bad(vec![output("../etc/x", b"a", copy())]), Code::Format);
        assert_eq!(
            bad(vec![
                output("lib/a", b"a", copy()),
                output("lib/a", b"a", copy())
            ]),
            Code::Format
        );
        assert_eq!(
            bad(vec![output(
                "lib/a",
                b"a",
                vec![step("copy@1", Some("a"), &[("extra", "1")])]
            )]),
            Code::Format
        );
        assert_eq!(
            bad(vec![output(
                "lib/a",
                b"a",
                vec![step("copy@1", Some("missing"), &[])]
            )]),
            Code::Format
        );
        assert_eq!(
            bad(vec![output("lib/a", b"a", vec![step("copy@1", None, &[])])]),
            Code::Format
        );
        let mac = vec![step(
            "trx-add-nvram@1",
            Some("a"),
            &[("nvram", "a"), ("placeholder_mac", "11:22:33:44:55:66")],
        )];
        assert_eq!(bad(vec![output("lib/a", b"a", mac)]), Code::Format);
    }
}
