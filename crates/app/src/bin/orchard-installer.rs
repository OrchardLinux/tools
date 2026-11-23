//! The installer: a distro's stamped installer (stamp.rs) installs that system.
fn main() {
    orchard_installer_app::run(orchard_installer_app::Kind::Installer)
}
