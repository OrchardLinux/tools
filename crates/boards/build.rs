use std::{env, fs, path::Path};

fn compile(name: &str, root: &Path, dirs: &[&str], includes: &[&str]) {
    let out = Path::new(&env::var_os("OUT_DIR").unwrap()).join(name);
    let compiler = cc::Build::new().get_compiler();
    let mut command = compiler.to_command();
    command.args(["-O2", "-w", "-D_GNU_SOURCE"]);
    for dir in includes {
        command.arg("-I").arg(root.join(dir));
    }
    for dir in dirs {
        let mut sources: Vec<_> = fs::read_dir(root.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "c"))
            .collect();
        sources.sort();
        command.args(sources);
    }
    assert!(
        command.arg("-o").arg(out).status().unwrap().success(),
        "building {name}"
    );
}

fn main() {
    let vendor = Path::new("../../vendor");
    println!("cargo:rerun-if-changed=../../vendor");
    compile(
        "hbootpatcher",
        &vendor.join("hbootpatcher"),
        &[
            "src",
            "patches",
            "plooshfinder/src",
            "plooshfinder/src/asm",
            "plooshfinder/src/formats",
        ],
        &["include", "plooshfinder/include"],
    );
    compile("iboot32patcher", &vendor.join("iboot32"), &["."], &["."]);
}
