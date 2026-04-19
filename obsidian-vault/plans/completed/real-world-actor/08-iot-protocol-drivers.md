---
title: "Phase 8: IoT Protocol Drivers"
tags:
  - plan
  - real-world
  - hardware
  - iot
  - mqtt
  - phase-8
date: 2026-04-08
status: complete
effort: 2d
priority: low
---

# Phase 8: IoT Protocol Drivers

> Extend `agentos-hal` with MQTT and REST/Home Assistant drivers so agents can discover and interact with IoT sensors and actuators.

---

## Why This Phase

The existing `agentos-hal` has 15+ drivers for local hardware (CPU, GPU, disk, network, sensors, etc.), but they all query the local machine. For IoT and home automation use cases, agents need to:

- **Subscribe to MQTT topics** from sensor networks
- **Publish MQTT messages** to actuators
- **Query Home Assistant** REST API for smart home device state
- **React to sensor readings** (temperature, motion, humidity) via the event system

The HAL's `HalDriver` trait, `HardwareRegistry`, and device approval workflow already exist — these new drivers plug directly into that architecture.

---

## Current State

- `HalDriver` trait: `name()`, `required_permission()`, `query(params)`, `device_key(params)`
- `HardwareRegistry`: device registration, approval, quarantine, per-agent access control
- `HalEventSink`: driver events emitted to kernel
- `DeviceAccessGate`: permission checks before hardware access
- 15+ existing drivers (system, process, network, storage, GPU, sensor, audio, bluetooth, etc.)
- No MQTT, CoAP, or REST-based IoT protocol support
- No external device discovery (only local hardware via `discover_available_devices()`)

## Target State

- `MqttDriver` — connects to MQTT broker, subscribes to topics, publishes messages
- `HomeAssistantDriver` — queries Home Assistant REST API for device state and control
- Both drivers register discovered devices in `HardwareRegistry`
- MQTT sensor readings emit `HardwareEvents` via `HalEventSink`
- Feature-gated: `mqtt` and `homeassistant` features in `agentos-hal`
- New agent tools: `iot-mqtt-publish`, `iot-mqtt-subscribe`, `iot-ha-state`, `iot-ha-call-service`

---

## Detailed Subtasks

### 1. MQTT driver

**File:** `crates/agentos-hal/src/drivers/mqtt.rs` (new)

```rust
use rumqttc::{AsyncClient, MqttOptions, QoS, Event, Packet};

pub struct MqttDriver {
    client: AsyncClient,
    event_loop: Arc<Mutex<EventLoop>>,  // managed separately
    broker_url: String,
    subscriptions: RwLock<HashMap<String, MqttSubscription>>,
    event_sink: Option<Arc<dyn HalEventSink>>,
}

#[derive(Debug, Clone)]
pub struct MqttSubscription {
    pub topic: String,
    pub qos: QoS,
    pub agent_id: AgentID,
    pub device_key: String,           // registered in HardwareRegistry
}

impl MqttDriver {
    pub async fn new(
        broker_url: &str,
        client_id: &str,
        credentials: Option<(String, String)>,
    ) -> Result<Self, AgentOSError>;

    /// Start the MQTT event loop (background task)
    /// Receives messages and:
    /// 1. Updates device state in registry
    /// 2. Emits HardwareEvents via event sink
    pub async fn start_listener(&self, cancel: CancellationToken) -> JoinHandle<()>;
}

#[async_trait]
impl HalDriver for MqttDriver {
    fn name(&self) -> &str { "mqtt" }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.mqtt", PermissionOp::Query)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        // "publish" action → Write, "subscribe"/"status" → Read
        match params.get("action").and_then(|v| v.as_str()) {
            Some("publish") => ("hardware.mqtt", PermissionOp::Write),
            _ => ("hardware.mqtt", PermissionOp::Read),
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match params.get("action").and_then(|v| v.as_str()) {
            Some("subscribe") => {
                let topic = params["topic"].as_str().ok_or(...)?;
                self.client.subscribe(topic, QoS::AtLeastOnce).await?;
                // Register device in HardwareRegistry
                Ok(json!({"subscribed": topic}))
            }
            Some("publish") => {
                let topic = params["topic"].as_str().ok_or(...)?;
                let payload = params["payload"].to_string();
                self.client.publish(topic, QoS::AtLeastOnce, false, payload).await?;
                Ok(json!({"published": topic}))
            }
            Some("status") => {
                // Return current subscriptions and last-known values
                Ok(json!({"subscriptions": self.list_subscriptions()}))
            }
            _ => Err(AgentOSError::InvalidInput("unknown MQTT action".into())),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        params.get("topic").and_then(|v| v.as_str()).map(|t| format!("mqtt:{}", t))
    }
}
```

### 2. Home Assistant driver

**File:** `crates/agentos-hal/src/drivers/homeassistant.rs` (new)

```rust
pub struct HomeAssistantDriver {
    base_url: String,                  // "http://homeassistant.local:8123"
    token_vault_key: String,           // vault key for long-lived access token
    http_client: reqwest::Client,
}

impl HomeAssistantDriver {
    pub fn new(base_url: &str, token_vault_key: &str) -> Self;
}

#[async_trait]
impl HalDriver for HomeAssistantDriver {
    fn name(&self) -> &str { "homeassistant" }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.homeassistant", PermissionOp::Query)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match params.get("action").and_then(|v| v.as_str()) {
            Some("list_entities") => {
                // GET /api/states
                // Return list of entity_ids with current state
            }
            Some("get_state") => {
                // GET /api/states/{entity_id}
                let entity_id = params["entity_id"].as_str().ok_or(...)?;
                // Return entity state + attributes
            }
            Some("call_service") => {
                // POST /api/services/{domain}/{service}
                let domain = params["domain"].as_str().ok_or(...)?;
                let service = params["service"].as_str().ok_or(...)?;
                let data = params.get("data").cloned().unwrap_or(json!({}));
                // e.g., domain="light", service="turn_on", data={"entity_id": "light.kitchen"}
            }
            _ => Err(AgentOSError::InvalidInput("unknown HA action".into())),
        }
    }
}
```

### 3. Feature gates

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[features]
mqtt = ["rumqttc"]
homeassistant = ["reqwest"]

[dependencies]
rumqttc = { version = "0.24", optional = true }
reqwest = { version = "0.12", features = ["json"], optional = true }
```

### 4. Registration at boot

**File:** `crates/agentos-hal/src/hal.rs`

In `new_with_defaults()` or a new `register_iot_drivers()` function:

```rust
#[cfg(feature = "mqtt")]
{
    if let Some(mqtt_config) = config.mqtt {
        let mqtt = MqttDriver::new(&mqtt_config.broker_url, &mqtt_config.client_id, creds).await?;
        hal.register(Box::new(mqtt));
    }
}

#[cfg(feature = "homeassistant")]
{
    if let Some(ha_config) = config.homeassistant {
        let ha = HomeAssistantDriver::new(&ha_config.base_url, &ha_config.token_vault_key);
        hal.register(Box::new(ha));
    }
}
```

### 5. Configuration

**File:** `config/default.toml`

```toml
[hal.mqtt]
# broker_url = "mqtt://localhost:1883"
# client_id = "agentos"
# username = ""
# password_vault_key = "mqtt_password"

[hal.homeassistant]
# base_url = "http://homeassistant.local:8123"
# token_vault_key = "homeassistant_token"
```

### 6. Agent tool manifests

**File:** `tools/core/iot-mqtt-publish.toml`, `tools/core/iot-mqtt-subscribe.toml`, `tools/core/iot-ha-state.toml`, `tools/core/iot-ha-call-service.toml` (all new)

Each tool delegates to the HAL driver with appropriate action parameters.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/src/drivers/mqtt.rs` | **New** — MQTT driver via rumqttc |
| `crates/agentos-hal/src/drivers/homeassistant.rs` | **New** — Home Assistant REST driver |
| `crates/agentos-hal/src/drivers/mod.rs` | Add feature-gated module declarations |
| `crates/agentos-hal/src/hal.rs` | Register IoT drivers at boot (feature-gated) |
| `crates/agentos-hal/Cargo.toml` | Add `mqtt` and `homeassistant` features + deps |
| `config/default.toml` | Add `[hal.mqtt]` and `[hal.homeassistant]` sections (commented out) |
| `tools/core/iot-mqtt-publish.toml` | **New** — Tool manifest |
| `tools/core/iot-mqtt-subscribe.toml` | **New** — Tool manifest |
| `tools/core/iot-ha-state.toml` | **New** — Tool manifest |
| `tools/core/iot-ha-call-service.toml` | **New** — Tool manifest |

---

## Dependencies

- **Requires:** None (independent track)
- **Blocks:** Phase 9 (Device Twin & Safety Engine)

---

## Test Plan

1. **Unit: MQTT driver query routing** — Verify `action=subscribe/publish/status` dispatches correctly
2. **Unit: HA driver query routing** — Verify `action=list_entities/get_state/call_service` dispatches correctly
3. **Unit: device_key generation** — Verify MQTT topic → `mqtt:sensors/temperature` device key
4. **Unit: permission mapping** — Verify `publish` → Write, `subscribe` → Read
5. **Integration: MQTT** (requires broker, e.g., mosquitto in Docker)
   - Subscribe to `test/topic`
   - Publish message to `test/topic`
   - Verify message received and device registered in HardwareRegistry
6. **Integration: Home Assistant** (mock with wiremock)
   - List entities → verify parsed response
   - Get state → verify entity state returned
   - Call service → verify POST sent with correct body
7. **Feature gate: no-default** — Build without `mqtt`/`homeassistant` features → compiles clean

---

## Verification

```bash
cargo build -p agentos-hal
cargo build -p agentos-hal --features mqtt,homeassistant
cargo test -p agentos-hal
cargo clippy -p agentos-hal -- -D warnings
cargo fmt -p agentos-hal -- --check
```
