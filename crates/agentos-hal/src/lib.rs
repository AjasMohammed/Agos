//! **Experimental.** Hardware abstraction layer. Driver surface and consent gates may change before v2; not covered by the v1 stability promise.

pub mod consent;
pub mod detect;
pub mod drivers;
pub mod hal;
pub mod registry;
pub mod safety;
pub mod twin;
pub mod types;

pub use consent::ConsentStore;
pub use detect::{
    probe_peripherals, probe_peripherals_with, PeripheralProbe, ProbeEnv, PERIPHERAL_DRIVERS,
};
pub use hal::{
    discover_available_devices, DeviceAccessGate, DiscoveredDevice, HalDriver, HalEventSink,
    HalOperation, HardwareAbstractionLayer,
};
pub use registry::{DeviceEntry, DeviceStatus, HardwareRegistry};
pub use safety::{SafetyEngine, SafetyRule, SafetyViolation};
pub use twin::{DeviceTwin, TwinRegistry};
pub use types::*;
