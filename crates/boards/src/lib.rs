//! What a board needs and how to make it without a device: catalog,
//! firmware archives, boot and peripheral firmware, firmware keys and
//! loaders, and packing what a loader is sent.
pub mod boot_firmware;
pub mod catalog;
pub mod data;
pub mod firmware_build;
pub mod install_env;
pub mod install_plan;
pub mod kernel_package;
pub mod pack;
pub mod peripheral;
pub mod release;
