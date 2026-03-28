pub mod drivers;
pub mod hal;
pub mod registry;
pub mod types;

pub use hal::{
    discover_available_devices, DeviceAccessGate, DiscoveredDevice, HalDriver, HalEventSink,
    HalOperation, HardwareAbstractionLayer,
};
pub use registry::{DeviceEntry, DeviceStatus, HardwareRegistry};
pub use types::*;
