pub mod consent;
pub mod drivers;
pub mod hal;
pub mod registry;
pub mod safety;
pub mod twin;
pub mod types;

pub use consent::ConsentStore;
pub use hal::{
    discover_available_devices, DeviceAccessGate, DiscoveredDevice, HalDriver, HalEventSink,
    HalOperation, HardwareAbstractionLayer,
};
pub use registry::{DeviceEntry, DeviceStatus, HardwareRegistry};
pub use safety::{SafetyEngine, SafetyRule, SafetyViolation};
pub use twin::{DeviceTwin, TwinRegistry};
pub use types::*;
