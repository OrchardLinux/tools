//! Wraps the 32-bit m1n1 for `bootx`, as `orchard2` does: for comparing
//! with orchard's `producers/m1n1_armv7.py`.
//! `m1n1_arm_image M1N1 OUT [BOOTARGS DTB KERNEL INITRAMFS]`
use orchard_formats::m1n1;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let read = |i: usize| std::fs::read(&args[i]).unwrap();
    let macho = read(1);
    let packed = match args.len() {
        3 => macho,
        _ => {
            let (dtb, kernel, initramfs) = (read(4), read(5), read(6));
            let dtbs = [dtb.as_slice()];
            let payload = m1n1::Payload {
                logo: None,
                bootargs: Some(&args[3]),
                dtbs: &dtbs,
                kernel: &kernel,
                initramfs: Some(&initramfs),
            };
            m1n1::pack(&macho, &payload).unwrap()
        }
    };
    std::fs::write(&args[2], m1n1::kernel_image_arm(&packed).unwrap()).unwrap();
}
