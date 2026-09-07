// Peripheral drivers below are Linux-only (v4l, bluer, zbus, PipeWire/CUPS
// CLIs, sysfs): they are gated on BOTH their feature flag and target_os, so
// enabling a feature off-Linux still compiles. Off-Linux the driver is simply
// not registered and `HardwareAbstractionLayer::query` returns a
// "Driver '…' not found" error — that is the graceful-degradation stub.
#[cfg(all(feature = "audio", target_os = "linux"))]
pub mod audio;
#[cfg(all(feature = "bluetooth", target_os = "linux"))]
pub mod bluetooth;
#[cfg(all(feature = "display", target_os = "linux"))]
pub mod display;
pub mod gpu;
#[cfg(feature = "homeassistant")]
pub mod homeassistant;
pub mod log_reader;
pub mod mounts;
#[cfg(feature = "mqtt")]
pub mod mqtt;
pub mod network;
pub mod network_sockets;
pub mod open_files;
#[cfg(all(feature = "printer", target_os = "linux"))]
pub mod printer;
pub mod process;
#[cfg(all(feature = "raw-usb", target_os = "linux"))]
pub mod raw_usb;
pub mod sensor;
pub mod services;
pub mod storage;
pub mod system;
#[cfg(all(feature = "usb-storage", target_os = "linux"))]
pub mod usb_storage;
#[cfg(all(feature = "webcam", target_os = "linux"))]
pub mod webcam;
