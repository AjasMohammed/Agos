use std::collections::BTreeSet;
use std::time::Duration;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use bluer::{Adapter, AdapterEvent, Address, Device, Session};
use futures::{pin_mut, StreamExt};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::hal::HalDriver;

const BLUETOOTH_DEVICE_PREFIX: &str = "bluetooth:";
const DEFAULT_SCAN_DURATION_SECONDS: u64 = 10;
const MAX_SCAN_DURATION_SECONDS: u64 = 30;
const MAX_GATT_WRITE_BYTES: usize = 512;
const RFKILL_ROOT: &str = "/sys/class/rfkill";
const RFKILL_DEVICE: &str = "/dev/rfkill";
/// `RFKILL_TYPE_BLUETOOTH` from `linux/rfkill.h`.
const RFKILL_TYPE_BLUETOOTH: u8 = 2;
/// `RFKILL_OP_CHANGE` from `linux/rfkill.h` — one switch, named by `idx`.
///
/// Not `RFKILL_OP_CHANGE_ALL`, which would clear every bluetooth switch on the
/// host. Powering on the adapter the caller named must not re-enable a second
/// adapter the operator deliberately blocked.
const RFKILL_OP_CHANGE: u8 = 2;
/// How long to keep retrying the power-on after clearing the switch.
/// ponytail: fixed poll, tune if a slower radio needs longer than 2s.
const RFKILL_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
const RFKILL_SETTLE_INTERVAL: Duration = Duration::from_millis(100);

/// One `/sys/class/rfkill/rfkillN` switch of `type = bluetooth`.
///
/// BlueZ answers a power-on against a blocked switch with
/// `org.bluez.Error.Busy`, which reads as "another operation holds the
/// adapter" and sends callers off retrying a state that never clears on its
/// own. Reading the switch lets the driver say what is actually wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RfkillSwitch {
    /// `/sys/class/rfkill/rfkillN/index` — the `idx` a `struct rfkill_event`
    /// needs to address this one switch.
    index: u32,
    name: String,
    soft_blocked: bool,
    hard_blocked: bool,
}

impl RfkillSwitch {
    fn is_blocked(&self) -> bool {
        self.soft_blocked || self.hard_blocked
    }

    /// A switch named after an adapter blocks only that adapter. One named
    /// after anything else (`ideapad_bluetooth`, say) is a platform-wide kill
    /// switch and blocks every adapter, so `hci1` is not reported blocked
    /// merely because `hci0`'s own switch is off.
    fn blocks_adapter(&self, adapter: &str, known_adapters: &BTreeSet<String>) -> bool {
        self.name == adapter || !known_adapters.contains(&self.name)
    }
}

/// Reads the bluetooth rfkill switches from sysfs.
///
/// Sync `std::fs` on purpose: these are memory-backed pseudo-files, and it
/// matches the existing sysfs readers in `hal.rs`, `gpu.rs`, and `sensor.rs`.
/// A missing or unreadable `/sys/class/rfkill` yields an empty list — the hint
/// is a diagnostic, never a gate.
fn bluetooth_rfkill_switches() -> Vec<RfkillSwitch> {
    bluetooth_rfkill_switches_in(std::path::Path::new(RFKILL_ROOT))
}

/// [`bluetooth_rfkill_switches`] against an arbitrary root, so the parser's
/// silent-failure branches are testable without real hardware.
fn bluetooth_rfkill_switches_in(root: &std::path::Path) -> Vec<RfkillSwitch> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let read_flag = |path: std::path::PathBuf| -> bool {
        std::fs::read_to_string(path)
            .map(|raw| raw.trim() == "1")
            .unwrap_or(false)
    };

    let mut switches = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let kind = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
        if kind.trim() != "bluetooth" {
            continue;
        }
        let Ok(name) = std::fs::read_to_string(dir.join("name")) else {
            continue;
        };
        // No index means no way to address the switch for a write. Skipping
        // beats keeping an entry that can be reported but never cleared.
        let Some(index) = std::fs::read_to_string(dir.join("index"))
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
        else {
            continue;
        };
        switches.push(RfkillSwitch {
            index,
            name: name.trim().to_string(),
            soft_blocked: read_flag(dir.join("soft")),
            hard_blocked: read_flag(dir.join("hard")),
        });
    }
    switches.sort_by(|a, b| a.name.cmp(&b.name));
    switches
}

/// Suffix appended to BlueZ power failures, empty when nothing is blocked.
fn rfkill_block_hint(switches: &[RfkillSwitch]) -> String {
    let soft: Vec<&str> = switches
        .iter()
        .filter(|s| s.soft_blocked)
        .map(|s| s.name.as_str())
        .collect();
    let hard: Vec<&str> = switches
        .iter()
        .filter(|s| s.hard_blocked)
        .map(|s| s.name.as_str())
        .collect();
    if soft.is_empty() && hard.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    if !soft.is_empty() {
        parts.push(format!("soft-blocked: {}", soft.join(", ")));
    }
    if !hard.is_empty() {
        parts.push(format!("hard-blocked: {}", hard.join(", ")));
    }
    let remedy = if hard.is_empty() {
        "run 'rfkill unblock bluetooth'"
    } else {
        "a hard block is a physical switch or firmware toggle and cannot be cleared from software"
    };
    format!(
        " (bluetooth is rfkill-blocked: {} \u{2014} {remedy}; BlueZ reports this as 'Busy')",
        parts.join("; ")
    )
}

/// The `struct rfkill_event` that unblocks one switch.
///
/// `{ __u32 idx; __u8 type, op, soft, hard; } __packed`, native-endian `idx`.
/// `hard` is read-only to userspace and ignored on write.
///
/// The 8-byte layout is the stable one. `linux/rfkill.h` records that
/// `rfkill_event` was rolled back to its original size after an ABI misstep,
/// with the longer `rfkill_event_ext` gated behind `RFKILL_IOCTL_MAX_SIZE`, so
/// this write needs no ioctl and no version check.
fn rfkill_unblock_event(index: u32) -> [u8; 8] {
    let mut event = [0u8; 8];
    event[..4].copy_from_slice(&index.to_ne_bytes());
    event[4] = RFKILL_TYPE_BLUETOOTH;
    event[5] = RFKILL_OP_CHANGE;
    event[6] = 0; // soft: unblock
    event[7] = 0; // hard: ignored on write
    event
}

/// The switches whose soft block stands between `adapter` and a power-on, and
/// that clearing could actually fix. Empty means "do not touch rfkill".
///
/// Scoped to the named adapter via [`RfkillSwitch::blocks_adapter`], the same
/// helper `list_adapters` reports block state with. A switch belonging to some
/// *other* adapter is the operator's deliberate choice and is none of this
/// call's business — powering on `hci0` must never re-enable a `hci1` they
/// turned off.
///
/// Empty for a power-*off* (rfkill never refuses one) and whenever a relevant
/// hard block is present: the kernel ignores the `hard` field on write, so
/// `soft: 0` cannot clear one, and unblocking would only buy a second, more
/// confusing failure. The caller reports the hard block instead.
fn clearable_switches<'a>(
    switches: &'a [RfkillSwitch],
    adapter: &str,
    known_adapters: &BTreeSet<String>,
    enabled: bool,
) -> Vec<&'a RfkillSwitch> {
    // An empty adapter set means the enumeration failed. `blocks_adapter`
    // classifies anything not named after a known adapter as a platform-wide
    // switch, so an empty set would make every switch look like ours and turn
    // this into the host-wide unblock it exists to prevent. Fail closed.
    if !enabled || known_adapters.is_empty() {
        return Vec::new();
    }
    let relevant: Vec<&RfkillSwitch> = switches
        .iter()
        .filter(|switch| switch.blocks_adapter(adapter, known_adapters))
        .collect();
    if relevant.iter().any(|switch| switch.hard_blocked) {
        return Vec::new();
    }
    relevant
        .into_iter()
        .filter(|switch| switch.soft_blocked)
        .collect()
}

/// Clears the given **soft** blocks by writing one `struct rfkill_event` per
/// switch to `/dev/rfkill` — what `rfkill unblock <index>` does.
///
/// The driver clears these itself rather than telling the agent to shell out,
/// because `shell-exec` needs `process.exec:x`, which is withheld from every
/// agent by default. An agent holding `hardware.bluetooth.power:x` would
/// otherwise read an accurate diagnosis of a fixable problem and have no way
/// to act on it. Powering the radio on is the permission; clearing the switch
/// that forbids powering it on is part of that, not a wider one.
///
/// Logged before the write, not after: this mutates host radio state, and the
/// record has to survive the write failing halfway through a multi-switch set.
fn clear_soft_blocks(switches: &[&RfkillSwitch]) -> Result<(), AgentOSError> {
    use std::io::Write;

    tracing::warn!(
        switches = ?switches.iter().map(|switch| &switch.name).collect::<Vec<_>>(),
        device = RFKILL_DEVICE,
        "Clearing bluetooth rfkill soft block to satisfy a power-on"
    );

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(RFKILL_DEVICE)
        .map_err(|error| rfkill_write_error("open", &error))?;
    for switch in switches {
        file.write_all(&rfkill_unblock_event(switch.index))
            .map_err(|error| rfkill_write_error("write to", &error))?;
    }
    Ok(())
}

/// The failure message for a power-on that never settled after an unblock.
///
/// Split out so the elapsed-time arithmetic is testable: the useful number is
/// what the loop actually spent, not the configured budget.
fn settle_failure_message(elapsed: Duration, last_error: &str, hint: &str) -> String {
    format!(
        "Cleared the bluetooth rfkill block but the adapter still refused to \
         power on after {:.1}s: {last_error}{hint}",
        elapsed.as_secs_f32(),
    )
}

/// `/dev/rfkill` is `root:netdev` with a `systemd-logind` ACL for the *active
/// seat*, so a kernel running as a `systemd --user` unit can write it and one
/// running headless in system scope cannot. That distinction is invisible in a
/// bare "permission denied", so name the fix.
fn rfkill_write_error(verb: &str, error: &std::io::Error) -> AgentOSError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        AgentOSError::HalError(format!(
            "Cannot {verb} {RFKILL_DEVICE} to clear the bluetooth block: {error}. \
             The kernel process needs write access — add it to the 'netdev' group, \
             or install a udev rule: KERNEL==\"rfkill\", SUBSYSTEM==\"misc\", \
             GROUP=\"netdev\", MODE=\"0664\""
        ))
    } else {
        AgentOSError::HalError(format!(
            "Cannot {verb} {RFKILL_DEVICE} to clear the bluetooth block: {error}"
        ))
    }
}

/// BlueZ D-Bus Bluetooth driver designed for long-running agent workflows.
///
/// The JSON outputs intentionally include adapter, address, connection state,
/// and characteristic identifiers so a task can resume or reason about prior
/// results without hidden in-memory driver state.
pub struct BluetoothDriver;

impl Default for BluetoothDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl BluetoothDriver {
    pub fn new() -> Self {
        Self
    }

    fn action_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'action' param".into()))
    }

    fn normalized_scan_duration(&self, params: &Value) -> u64 {
        params
            .get("duration_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SCAN_DURATION_SECONDS)
            .min(MAX_SCAN_DURATION_SECONDS)
    }

    fn adapter_name_from_params<'a>(&self, params: &'a Value) -> Option<&'a str> {
        params
            .get("adapter")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    }

    fn normalized_bt_address(address: &str) -> Option<String> {
        let parts: Vec<_> = address.split(':').collect();
        if parts.len() != 6 {
            return None;
        }
        let mut normalized = Vec::with_capacity(6);
        for part in parts {
            if part.len() != 2 || !part.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            normalized.push(part.to_ascii_uppercase());
        }
        Some(normalized.join(":"))
    }

    fn parse_address_param(&self, params: &Value) -> Result<Address, AgentOSError> {
        let raw = params
            .get("address")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'address' param".into()))?;
        let normalized = Self::normalized_bt_address(raw).ok_or_else(|| {
            AgentOSError::HalError(
                "Invalid 'address' param: expected a Bluetooth MAC like 'AA:BB:CC:DD:EE:FF'".into(),
            )
        })?;
        normalized
            .parse()
            .map_err(|error| AgentOSError::HalError(format!("Invalid Bluetooth address: {error}")))
    }

    fn parse_uuid_param(&self, params: &Value, field_name: &str) -> Result<Uuid, AgentOSError> {
        let raw = params
            .get(field_name)
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError(format!("Missing '{field_name}' param")))?;
        Uuid::parse_str(raw).map_err(|error| {
            AgentOSError::HalError(format!("Invalid '{field_name}' UUID: {error}"))
        })
    }

    fn decode_write_value(&self, params: &Value) -> Result<Vec<u8>, AgentOSError> {
        if let Some(value) = params.get("value_base64").and_then(Value::as_str) {
            let bytes = BASE64_STANDARD.decode(value).map_err(|error| {
                AgentOSError::HalError(format!("Invalid 'value_base64' payload: {error}"))
            })?;
            if bytes.len() > MAX_GATT_WRITE_BYTES {
                return Err(AgentOSError::HalError(format!(
                    "GATT writes are limited to {MAX_GATT_WRITE_BYTES} bytes"
                )));
            }
            return Ok(bytes);
        }

        if let Some(values) = params.get("value").and_then(Value::as_array) {
            let mut bytes = Vec::with_capacity(values.len());
            for value in values {
                let byte = value.as_u64().ok_or_else(|| {
                    AgentOSError::HalError(
                        "Invalid 'value' payload: expected an array of byte integers".into(),
                    )
                })?;
                if byte > u8::MAX as u64 {
                    return Err(AgentOSError::HalError(
                        "Invalid 'value' payload: each byte must be between 0 and 255".into(),
                    ));
                }
                bytes.push(byte as u8);
            }
            if bytes.len() > MAX_GATT_WRITE_BYTES {
                return Err(AgentOSError::HalError(format!(
                    "GATT writes are limited to {MAX_GATT_WRITE_BYTES} bytes"
                )));
            }
            return Ok(bytes);
        }

        Err(AgentOSError::HalError(
            "Missing GATT payload: provide 'value_base64' or a 'value' byte array".into(),
        ))
    }

    async fn session_and_adapter(
        &self,
        params: &Value,
    ) -> Result<(Session, Adapter), AgentOSError> {
        let session = Session::new()
            .await
            .map_err(|error| AgentOSError::HalError(format!("BlueZ session failed: {error}")))?;

        let adapter = if let Some(name) = self.adapter_name_from_params(params) {
            session.adapter(name).map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth adapter '{name}' not found: {error}"))
            })?
        } else {
            session.default_adapter().await.map_err(|error| {
                AgentOSError::HalError(format!("No Bluetooth adapter available: {error}"))
            })?
        };

        Ok((session, adapter))
    }

    async fn device_from_params(
        &self,
        params: &Value,
    ) -> Result<(Session, Adapter, Device), AgentOSError> {
        let address = self.parse_address_param(params)?;
        let (session, adapter) = self.session_and_adapter(params).await?;
        let device = adapter.device(address).map_err(|error| {
            AgentOSError::HalError(format!(
                "Failed to access Bluetooth device '{}': {error}",
                address
            ))
        })?;
        Ok((session, adapter, device))
    }

    async fn ensure_adapter_powered(&self, adapter: &Adapter) -> Result<(), AgentOSError> {
        adapter.set_powered(true).await.map_err(|error| {
            let hint = rfkill_block_hint(&bluetooth_rfkill_switches());
            AgentOSError::HalError(format!("Failed to power adapter: {error}{hint}"))
        })
    }

    /// Powers the adapter, clearing an rfkill soft block first if that is what
    /// stands in the way.
    ///
    /// Only on an explicit power-on. `ensure_adapter_powered` deliberately
    /// does not do this: it serves `scan`, `connect` and `pair`, and those
    /// callers asked to use the radio, not to change its blocked state.
    async fn set_power(&self, params: &Value) -> Result<Value, AgentOSError> {
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| AgentOSError::HalError("Missing 'enabled' bool param".into()))?;
        let (session, adapter) = self.session_and_adapter(params).await?;

        let power_error = adapter.set_powered(enabled).await.err();

        let mut rfkill_cleared = false;
        if let Some(error) = power_error {
            let switches = bluetooth_rfkill_switches();
            let known_adapters: BTreeSet<String> = session
                .adapter_names()
                .await
                .map(|names| names.into_iter().collect())
                .unwrap_or_default();
            // The guard is load-bearing, not decorative: without it a power
            // failure with an unrelated cause would rewrite the host's kill
            // switches. Never call `clear_soft_blocks` on an unfiltered list.
            let clearable = clearable_switches(&switches, adapter.name(), &known_adapters, enabled);

            if clearable.is_empty() {
                let hint = rfkill_block_hint(&switches);
                return Err(AgentOSError::HalError(format!(
                    "Failed to set adapter power: {error}{hint}"
                )));
            }

            // Keep the BlueZ error and the block hint. When the write fails
            // it is usually EACCES on a headless kernel, and the operator
            // still wants the cheaper `rfkill unblock` remedy the hint names.
            if let Err(clear_error) = clear_soft_blocks(&clearable) {
                let hint = rfkill_block_hint(&switches);
                return Err(AgentOSError::HalError(format!(
                    "Failed to set adapter power: {error}{hint}. Clearing the block \
                     from the kernel also failed: {clear_error}"
                )));
            }
            rfkill_cleared = true;
            self.power_on_after_unblock(&adapter).await?;
        }

        Ok(json!({
            "adapter": adapter.name(),
            "powered": adapter.is_powered().await.unwrap_or(enabled),
            "rfkill_cleared": rfkill_cleared,
        }))
    }

    /// Retries the power-on while BlueZ catches up with the cleared switch.
    ///
    /// The kernel's rfkill uevent reaches `bluetoothd` asynchronously, and
    /// until it lands the adapter is still refusing with `Busy`. A single
    /// immediate retry loses that race on real hardware often enough to matter.
    ///
    /// Bounded by wall clock, not by attempt count: every `set_powered` is a
    /// D-Bus round trip that `bluer` lets run for up to 120s, so counting
    /// attempts would bound this at 40 minutes inside one tool call.
    async fn power_on_after_unblock(&self, adapter: &Adapter) -> Result<(), AgentOSError> {
        let started = tokio::time::Instant::now();
        let deadline = started + RFKILL_SETTLE_TIMEOUT;

        let last_error = loop {
            match adapter.set_powered(true).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if tokio::time::Instant::now() >= deadline {
                        break error;
                    }
                    tokio::time::sleep(RFKILL_SETTLE_INTERVAL).await;
                }
            }
        };

        // Re-read: the switch state now is what explains the failure, not the
        // state that prompted the unblock.
        Err(AgentOSError::HalError(settle_failure_message(
            started.elapsed(),
            &last_error.to_string(),
            &rfkill_block_hint(&bluetooth_rfkill_switches()),
        )))
    }

    async fn device_snapshot(
        &self,
        adapter_name: &str,
        device: &Device,
    ) -> Result<Value, AgentOSError> {
        let address = device.address().to_string();
        let uuids = device
            .uuids()
            .await
            .map_err(|error| {
                AgentOSError::HalError(format!("Failed to query device UUIDs: {error}"))
            })?
            .unwrap_or_default()
            .into_iter()
            .map(|uuid| uuid.to_string())
            .collect::<Vec<_>>();
        let manufacturer_data = device
            .manufacturer_data()
            .await
            .map_err(|error| {
                AgentOSError::HalError(format!("Failed to query manufacturer data: {error}"))
            })?
            .unwrap_or_default()
            .into_iter()
            .map(|(company_id, bytes)| {
                json!({
                    "company_id": company_id,
                    "data_base64": BASE64_STANDARD.encode(bytes),
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "adapter": adapter_name,
            "address": address,
            "name": device.name().await.ok().flatten(),
            "alias": device.alias().await.ok(),
            "icon": device.icon().await.ok().flatten(),
            "rssi": device.rssi().await.ok().flatten(),
            "paired": device.is_paired().await.unwrap_or(false),
            "connected": device.is_connected().await.unwrap_or(false),
            "trusted": device.is_trusted().await.unwrap_or(false),
            "blocked": device.is_blocked().await.unwrap_or(false),
            "uuids": uuids,
            "manufacturer_data": manufacturer_data,
        }))
    }

    async fn list_adapters(&self, params: &Value) -> Result<Value, AgentOSError> {
        let session = Session::new()
            .await
            .map_err(|error| AgentOSError::HalError(format!("BlueZ session failed: {error}")))?;
        let mut names = session
            .adapter_names()
            .await
            .map_err(|error| AgentOSError::HalError(format!("Failed to list adapters: {error}")))?;
        names.sort();

        let default_adapter = session
            .default_adapter()
            .await
            .ok()
            .map(|adapter| adapter.name().to_string());
        let include_properties = params
            .get("include_properties")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let known_adapters: BTreeSet<String> = names.iter().cloned().collect();
        let rfkill = bluetooth_rfkill_switches();

        let mut adapters = Vec::with_capacity(names.len());
        for name in names {
            let adapter = session.adapter(&name).map_err(|error| {
                AgentOSError::HalError(format!("Failed to load adapter '{name}': {error}"))
            })?;

            let mut entry = json!({
                "name": &name,
                "is_default": default_adapter.as_deref() == Some(adapter.name()),
            });

            if include_properties {
                entry["address"] = adapter
                    .address()
                    .await
                    .ok()
                    .map(|address| Value::String(address.to_string()))
                    .unwrap_or(Value::Null);
                entry["address_type"] = adapter
                    .address_type()
                    .await
                    .ok()
                    .map(|kind| Value::String(kind.to_string()))
                    .unwrap_or(Value::Null);
                entry["alias"] = adapter
                    .alias()
                    .await
                    .ok()
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                entry["powered"] = Value::Bool(adapter.is_powered().await.unwrap_or(false));
                // An rfkill-blocked adapter reports powered=false and refuses
                // every power-on with `org.bluez.Error.Busy`, so surface the
                // block or the caller has no way to tell the two apart.
                let blocking: Vec<&RfkillSwitch> = rfkill
                    .iter()
                    .filter(|switch| {
                        switch.is_blocked() && switch.blocks_adapter(&name, &known_adapters)
                    })
                    .collect();
                entry["blocked"] = Value::Bool(!blocking.is_empty());
                entry["soft_blocked"] =
                    Value::Bool(blocking.iter().any(|switch| switch.soft_blocked));
                entry["hard_blocked"] =
                    Value::Bool(blocking.iter().any(|switch| switch.hard_blocked));
                entry["discoverable"] =
                    Value::Bool(adapter.is_discoverable().await.unwrap_or(false));
                entry["pairable"] = Value::Bool(adapter.is_pairable().await.unwrap_or(false));
                entry["discovering"] = Value::Bool(adapter.is_discovering().await.unwrap_or(false));
            }

            adapters.push(entry);
        }

        Ok(json!({
            "adapters": adapters,
            "default_adapter": default_adapter,
        }))
    }

    /// Devices BlueZ already has a pairing for.
    ///
    /// Deliberately does *not* power the adapter: the pairing database lives in
    /// BlueZ, not the radio, so this answers with the radio off. Without it the
    /// only way to see paired devices was a `scan`, which needs the radio on
    /// and spends its full duration before returning.
    async fn list_paired(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter) = self.session_and_adapter(params).await?;

        let addresses = adapter.device_addresses().await.map_err(|error| {
            AgentOSError::HalError(format!("Failed to list known devices: {error}"))
        })?;

        let mut devices = Vec::new();
        for address in addresses {
            let device = adapter.device(address).map_err(|error| {
                AgentOSError::HalError(format!("Failed to open device '{address}': {error}"))
            })?;
            // Ask the cheap question first — a snapshot costs several D-Bus
            // round trips and most known devices are not paired.
            if !device.is_paired().await.unwrap_or(false) {
                continue;
            }
            // Skip rather than propagate: a device can vanish between
            // `device_addresses` and the snapshot, and one unreadable entry
            // must not cost the caller the whole pairing list.
            match self.device_snapshot(adapter.name(), &device).await {
                Ok(snapshot) => devices.push(snapshot),
                Err(error) => tracing::warn!(
                    address = %address,
                    error = %error,
                    "Skipping paired device that could not be read"
                ),
            }
        }

        Ok(json!({
            "adapter": adapter.name(),
            "devices": devices,
        }))
    }

    async fn scan_devices(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter) = self.session_and_adapter(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        let duration = self.normalized_scan_duration(params);
        let discover = adapter.discover_devices().await.map_err(|error| {
            AgentOSError::HalError(format!("Bluetooth discovery failed: {error}"))
        })?;

        let mut addresses = BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(duration);
        pin_mut!(discover);

        loop {
            tokio::select! {
                maybe_event = discover.next() => {
                    match maybe_event {
                        Some(AdapterEvent::DeviceAdded(address)) => {
                            addresses.insert(address);
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }

        let mut devices = Vec::with_capacity(addresses.len());
        for address in addresses {
            let device = adapter.device(address).map_err(|error| {
                AgentOSError::HalError(format!(
                    "Failed to reopen discovered device '{}': {error}",
                    address
                ))
            })?;
            devices.push(self.device_snapshot(adapter.name(), &device).await?);
        }

        Ok(json!({
            "adapter": adapter.name(),
            "devices": devices,
            "scan_duration_seconds": duration,
        }))
    }

    /// Pair with a Bluetooth device.
    ///
    /// SAFETY: Pairing is security-sensitive and requires kernel-level escalation
    /// approval before reaching this method. The kernel's DeviceAccessGate enforces
    /// this. If unexpected pairing occurs, check the escalation pipeline.
    async fn pair_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device) = self.device_from_params(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        tracing::info!(
            address = %device.address(),
            adapter = %adapter.name(),
            "Bluetooth pairing initiated (escalation assumed pre-approved by kernel)"
        );

        device.pair().await.map_err(|error| {
            AgentOSError::HalError(format!("Bluetooth pairing failed: {error}"))
        })?;

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "name": device.name().await.ok().flatten(),
            "paired": true,
            "connected": device.is_connected().await.unwrap_or(false),
        }))
    }

    async fn connect_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device) = self.device_from_params(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        if !device.is_connected().await.unwrap_or(false) {
            device.connect().await.map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth connect failed: {error}"))
            })?;
        }

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "name": device.name().await.ok().flatten(),
            "connected": device.is_connected().await.unwrap_or(true),
            "paired": device.is_paired().await.unwrap_or(false),
        }))
    }

    async fn disconnect_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device) = self.device_from_params(params).await?;

        if device.is_connected().await.unwrap_or(false) {
            device.disconnect().await.map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth disconnect failed: {error}"))
            })?;
        }

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "name": device.name().await.ok().flatten(),
            "connected": device.is_connected().await.unwrap_or(false),
        }))
    }

    async fn gatt_characteristic(
        &self,
        params: &Value,
    ) -> Result<
        (
            Session,
            Adapter,
            Device,
            bluer::gatt::remote::Characteristic,
            Uuid,
            Uuid,
        ),
        AgentOSError,
    > {
        let service_uuid = self.parse_uuid_param(params, "service_uuid")?;
        let characteristic_uuid = self.parse_uuid_param(params, "characteristic_uuid")?;
        let (session, adapter, device) = self.device_from_params(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        if !device.is_connected().await.unwrap_or(false) {
            device.connect().await.map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth connect for GATT failed: {error}"))
            })?;
        }

        for service in device.services().await.map_err(|error| {
            AgentOSError::HalError(format!("Failed to enumerate GATT services: {error}"))
        })? {
            let uuid = service.uuid().await.map_err(|error| {
                AgentOSError::HalError(format!("Failed to read GATT service UUID: {error}"))
            })?;
            if uuid != service_uuid {
                continue;
            }

            for characteristic in service.characteristics().await.map_err(|error| {
                AgentOSError::HalError(format!(
                    "Failed to enumerate GATT characteristics for service {service_uuid}: {error}"
                ))
            })? {
                let uuid = characteristic.uuid().await.map_err(|error| {
                    AgentOSError::HalError(format!(
                        "Failed to read GATT characteristic UUID for service {service_uuid}: {error}"
                    ))
                })?;
                if uuid == characteristic_uuid {
                    return Ok((
                        session,
                        adapter,
                        device,
                        characteristic,
                        service_uuid,
                        characteristic_uuid,
                    ));
                }
            }
        }

        Err(AgentOSError::HalError(format!(
            "GATT characteristic {characteristic_uuid} under service {service_uuid} was not found"
        )))
    }

    async fn gatt_read(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device, characteristic, service_uuid, characteristic_uuid) =
            self.gatt_characteristic(params).await?;
        let bytes = characteristic.read().await.map_err(|error| {
            AgentOSError::HalError(format!(
                "GATT read failed for {characteristic_uuid}: {error}"
            ))
        })?;

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "service_uuid": service_uuid.to_string(),
            "characteristic_uuid": characteristic_uuid.to_string(),
            "value_base64": BASE64_STANDARD.encode(&bytes),
            "value_len": bytes.len(),
            "connected": device.is_connected().await.unwrap_or(true),
        }))
    }

    async fn gatt_write(&self, params: &Value) -> Result<Value, AgentOSError> {
        let payload = self.decode_write_value(params)?;
        let (_session, adapter, device, characteristic, service_uuid, characteristic_uuid) =
            self.gatt_characteristic(params).await?;
        characteristic.write(&payload).await.map_err(|error| {
            AgentOSError::HalError(format!(
                "GATT write failed for {characteristic_uuid}: {error}"
            ))
        })?;

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "service_uuid": service_uuid.to_string(),
            "characteristic_uuid": characteristic_uuid.to_string(),
            "written_bytes": payload.len(),
            "connected": device.is_connected().await.unwrap_or(true),
        }))
    }
}

#[async_trait]
impl HalDriver for BluetoothDriver {
    fn name(&self) -> &str {
        "bluetooth"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.bluetooth.list", PermissionOp::Read)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list_adapters")
        {
            "list_adapters" | "list_paired" => ("hardware.bluetooth.list", PermissionOp::Read),
            "power" => ("hardware.bluetooth.power", PermissionOp::Execute),
            "scan" => ("hardware.bluetooth.scan", PermissionOp::Observe),
            "pair" => ("hardware.bluetooth.pair", PermissionOp::Execute),
            "connect" | "disconnect" => ("hardware.bluetooth.connection", PermissionOp::Execute),
            "gatt_read" => ("hardware.bluetooth.gatt", PermissionOp::Read),
            "gatt_write" => ("hardware.bluetooth.gatt", PermissionOp::Write),
            _ => ("hardware.bluetooth.list", PermissionOp::Read),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list_adapters")
        {
            "pair" | "connect" | "disconnect" | "gatt_read" | "gatt_write" => params
                .get("address")
                .and_then(Value::as_str)
                .and_then(Self::normalized_bt_address)
                .map(|address| format!("{BLUETOOTH_DEVICE_PREFIX}{address}")),
            // `set_power` needs only `enabled` — it defaults the adapter and
            // will clear rfkill soft blocks on failure. Keying it on `address`
            // (which it never reads) left powering the radio down, and
            // unblocking a host kill switch, outside the approval gate.
            "power" => Some(format!(
                "{BLUETOOTH_DEVICE_PREFIX}adapter:{}",
                params
                    .get("adapter")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
            )),
            _ => None,
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match self.action_from_params(&params)? {
            "list_adapters" => self.list_adapters(&params).await,
            "list_paired" => self.list_paired(&params).await,
            "power" => self.set_power(&params).await,
            "scan" => self.scan_devices(&params).await,
            "pair" => self.pair_device(&params).await,
            "connect" => self.connect_device(&params).await,
            "disconnect" => self.disconnect_device(&params).await,
            "gatt_read" => self.gatt_read(&params).await,
            "gatt_write" => self.gatt_write(&params).await,
            action => Err(AgentOSError::HalError(format!(
                "Unsupported bluetooth action '{action}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_duration_is_capped() {
        let driver = BluetoothDriver::new();
        assert_eq!(driver.normalized_scan_duration(&json!({})), 10);
        assert_eq!(
            driver.normalized_scan_duration(&json!({ "duration_seconds": 120 })),
            30
        );
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        let driver = BluetoothDriver::new();
        let err = driver
            .parse_address_param(&json!({ "address": "not-a-mac" }))
            .expect_err("invalid address should fail");
        assert!(
            matches!(err, AgentOSError::HalError(message) if message.contains("Invalid 'address'"))
        );
    }

    #[test]
    fn device_key_is_stable_for_device_actions() {
        let driver = BluetoothDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "action": "connect", "address": "aa:bb:cc:dd:ee:ff" })),
            Some("bluetooth:AA:BB:CC:DD:EE:FF".to_string())
        );
        assert_eq!(driver.device_key(&json!({ "action": "scan" })), None);
        // `power` changes adapter state with no `address`; it must still gate.
        assert_eq!(
            driver.device_key(&json!({ "action": "power", "enabled": false })),
            Some("bluetooth:adapter:default".to_string())
        );
        assert_eq!(
            driver.device_key(&json!({ "action": "power", "enabled": true, "adapter": "hci1" })),
            Some("bluetooth:adapter:hci1".to_string())
        );
        // Reading the pairing database names no single device, so it must not
        // be routed through the per-device approval gate.
        assert_eq!(driver.device_key(&json!({ "action": "list_paired" })), None);
    }

    #[test]
    fn write_payload_accepts_base64_or_byte_array() {
        let driver = BluetoothDriver::new();
        assert_eq!(
            driver
                .decode_write_value(&json!({ "value": [1, 2, 3] }))
                .unwrap(),
            vec![1, 2, 3]
        );
        assert_eq!(
            driver
                .decode_write_value(&json!({ "value_base64": "AQID" }))
                .unwrap(),
            vec![1, 2, 3]
        );
    }

    fn switch(name: &str, soft: bool, hard: bool) -> RfkillSwitch {
        RfkillSwitch {
            index: 0,
            name: name.to_string(),
            soft_blocked: soft,
            hard_blocked: hard,
        }
    }

    #[test]
    fn platform_switch_blocks_every_adapter() {
        // `ideapad_bluetooth` names no adapter, so it is a platform-wide kill
        // switch and takes down hci0 and hci1 alike.
        let adapters: BTreeSet<String> = ["hci0", "hci1"].iter().map(|s| s.to_string()).collect();
        let platform = switch("ideapad_bluetooth", true, false);
        assert!(platform.blocks_adapter("hci0", &adapters));
        assert!(platform.blocks_adapter("hci1", &adapters));
    }

    #[test]
    fn named_switch_blocks_only_its_own_adapter() {
        let adapters: BTreeSet<String> = ["hci0", "hci1"].iter().map(|s| s.to_string()).collect();
        let hci0 = switch("hci0", true, false);
        assert!(hci0.blocks_adapter("hci0", &adapters));
        assert!(!hci0.blocks_adapter("hci1", &adapters));
    }

    #[test]
    fn rfkill_hint_is_empty_when_nothing_is_blocked() {
        assert!(rfkill_block_hint(&[]).is_empty());
        assert!(rfkill_block_hint(&[switch("hci0", false, false)]).is_empty());
    }

    #[test]
    fn rfkill_hint_names_blocked_switches_and_remedy() {
        let hint = rfkill_block_hint(&[
            switch("ideapad_bluetooth", true, false),
            switch("hci0", true, false),
        ]);
        assert!(
            hint.contains("soft-blocked: ideapad_bluetooth, hci0"),
            "{hint}"
        );
        assert!(hint.contains("rfkill unblock bluetooth"), "{hint}");
        assert!(!hint.contains("hard-blocked"), "{hint}");
    }

    #[test]
    fn rfkill_hint_flags_hard_blocks_as_unclearable() {
        let hint = rfkill_block_hint(&[switch("hci0", false, true)]);
        assert!(hint.contains("hard-blocked: hci0"), "{hint}");
        // A hard block is a physical switch; telling the caller to run
        // `rfkill unblock` would send them after a fix that cannot work.
        assert!(!hint.contains("rfkill unblock"), "{hint}");
        assert!(hint.contains("physical switch"), "{hint}");
    }

    #[test]
    fn unblock_event_matches_the_kernel_abi() {
        // struct rfkill_event { __u32 idx; __u8 type, op, soft, hard; } __packed
        // from linux/rfkill.h: RFKILL_TYPE_BLUETOOTH = 2, RFKILL_OP_CHANGE = 2.
        // The 8-byte layout is the stable one; rfkill_event_ext is opt-in via
        // RFKILL_IOCTL_MAX_SIZE, so this write needs no ioctl.
        let mut expected = [0u8; 8];
        expected[..4].copy_from_slice(&7u32.to_ne_bytes());
        expected[4] = 2;
        expected[5] = 2;
        assert_eq!(rfkill_unblock_event(7), expected);
    }

    #[test]
    fn unblock_event_never_uses_change_all() {
        // CHANGE_ALL (3) would clear every bluetooth switch on the host,
        // including adapters the operator blocked on purpose.
        assert_eq!(rfkill_unblock_event(0)[5], RFKILL_OP_CHANGE);
        assert_ne!(rfkill_unblock_event(0)[5], 3);
    }

    fn known(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn cleared_names(switches: &[&RfkillSwitch]) -> Vec<String> {
        switches.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn a_soft_block_on_this_adapter_is_clearable() {
        let switches = [switch("hci0", true, false)];
        let clearable = clearable_switches(&switches, "hci0", &known(&["hci0"]), true);
        assert_eq!(cleared_names(&clearable), ["hci0"]);
    }

    #[test]
    fn a_platform_switch_is_clearable_for_any_adapter() {
        // `ideapad_bluetooth` is named after no adapter, so it gates them all.
        let switches = [switch("ideapad_bluetooth", true, false)];
        let clearable = clearable_switches(&switches, "hci0", &known(&["hci0", "hci1"]), true);
        assert_eq!(cleared_names(&clearable), ["ideapad_bluetooth"]);
    }

    #[test]
    fn a_block_on_another_adapter_is_not_ours_to_clear() {
        // The operator blocked the dongle on purpose. Powering on the builtin
        // adapter must not put it back on the air.
        let switches = [switch("hci1", true, false)];
        let clearable = clearable_switches(&switches, "hci0", &known(&["hci0", "hci1"]), true);
        assert!(clearable.is_empty(), "{:?}", cleared_names(&clearable));
    }

    #[test]
    fn a_hard_block_is_never_clearable() {
        // Even alongside a soft block: `soft: 0` cannot lift a hard block, so
        // unblocking would only buy a second, more confusing failure.
        let switches = [switch("hci0", true, false), switch("ideapad", false, true)];
        let clearable = clearable_switches(&switches, "hci0", &known(&["hci0"]), true);
        assert!(clearable.is_empty());
    }

    #[test]
    fn another_adapters_hard_block_does_not_veto_ours() {
        // `hci1` is hard-blocked, but that switch is not in `hci0`'s way.
        let switches = [switch("hci0", true, false), switch("hci1", false, true)];
        let clearable = clearable_switches(&switches, "hci0", &known(&["hci0", "hci1"]), true);
        assert_eq!(cleared_names(&clearable), ["hci0"]);
    }

    #[test]
    fn powering_off_never_clears_a_block() {
        let switches = [switch("hci0", true, false)];
        assert!(clearable_switches(&switches, "hci0", &known(&["hci0"]), false).is_empty());
    }

    #[test]
    fn an_unblocked_or_unreadable_switch_set_is_not_clearable() {
        // Nothing to clear means the power failure has another cause, and
        // rewriting rfkill would hide it.
        let unblocked = [switch("hci0", false, false)];
        assert!(clearable_switches(&unblocked, "hci0", &known(&["hci0"]), true).is_empty());
        assert!(clearable_switches(&[], "hci0", &known(&["hci0"]), true).is_empty());
    }

    #[test]
    fn an_unknown_adapter_set_fails_closed() {
        // Enumeration failed. Without it every switch looks platform-wide,
        // which is exactly the host-wide unblock this scoping prevents.
        let switches = [switch("hci0", true, false)];
        assert!(clearable_switches(&switches, "hci0", &BTreeSet::new(), true).is_empty());
    }

    #[test]
    fn an_eacces_on_dev_rfkill_names_the_remedy() {
        // The seat ACL on /dev/rfkill is why this works under a `systemd --user`
        // kernel and not a headless one — invisible in a bare "permission denied".
        let error = rfkill_write_error(
            "open",
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        let message = error.to_string();
        assert!(message.contains("netdev"), "{message}");
        assert!(message.contains("udev rule"), "{message}");
    }

    #[test]
    fn a_non_permission_error_stays_plain() {
        let error = rfkill_write_error("open", &std::io::Error::from(std::io::ErrorKind::NotFound));
        let message = error.to_string();
        assert!(message.contains("/dev/rfkill"), "{message}");
        assert!(!message.contains("netdev"), "{message}");
    }

    #[test]
    fn settle_failure_reports_elapsed_time_and_keeps_the_hint() {
        let message = settle_failure_message(
            Duration::from_millis(1900),
            "Busy",
            " (bluetooth is rfkill-blocked: soft-blocked: hci0)",
        );
        // The number a human wants is what the loop spent, not the budget.
        assert!(message.contains("1.9s"), "{message}");
        assert!(message.contains("Busy"), "{message}");
        assert!(message.contains("soft-blocked: hci0"), "{message}");
    }

    fn write_switch(root: &std::path::Path, dir: &str, attrs: &[(&str, &str)]) {
        let path = root.join(dir);
        std::fs::create_dir_all(&path).unwrap();
        for (name, value) in attrs {
            std::fs::write(path.join(name), value).unwrap();
        }
    }

    #[test]
    fn sysfs_parser_reads_bluetooth_switches_and_skips_the_rest() {
        let root = tempfile::tempdir().unwrap();
        // Trailing newlines are how sysfs actually renders these.
        write_switch(
            root.path(),
            "rfkill2",
            &[
                ("type", "bluetooth\n"),
                ("name", "hci0\n"),
                ("index", "2\n"),
                ("soft", "1\n"),
                ("hard", "0\n"),
            ],
        );
        // Wrong type: a wifi switch must never reach a bluetooth unblock.
        write_switch(
            root.path(),
            "rfkill0",
            &[
                ("type", "wlan\n"),
                ("name", "phy0\n"),
                ("index", "0\n"),
                ("soft", "1\n"),
                ("hard", "0\n"),
            ],
        );
        // No index: reportable but not addressable for a write, so dropped.
        write_switch(
            root.path(),
            "rfkill9",
            &[
                ("type", "bluetooth\n"),
                ("name", "ghost\n"),
                ("soft", "1\n"),
                ("hard", "0\n"),
            ],
        );

        let switches = bluetooth_rfkill_switches_in(root.path());
        assert_eq!(switches.len(), 1, "{switches:?}");
        assert_eq!(switches[0].index, 2);
        assert_eq!(switches[0].name, "hci0");
        assert!(switches[0].soft_blocked);
        assert!(!switches[0].hard_blocked);
    }

    #[test]
    fn a_missing_sysfs_root_yields_no_switches() {
        // The hint is a diagnostic, never a gate — and an empty set is also
        // what stops `clearable_switches` from authorising a write.
        assert!(
            bluetooth_rfkill_switches_in(std::path::Path::new("/nonexistent/rfkill")).is_empty()
        );
    }

    #[test]
    fn permissions_are_action_scoped() {
        let driver = BluetoothDriver::new();
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "power", "enabled": false })),
            ("hardware.bluetooth.power", PermissionOp::Execute)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "scan" })),
            ("hardware.bluetooth.scan", PermissionOp::Observe)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "gatt_write" })),
            ("hardware.bluetooth.gatt", PermissionOp::Write)
        );
    }
}
