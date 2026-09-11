//! WiFi management backed by NetworkManager's `nmcli`.
//!
//! Shelling out to `nmcli` rather than driving the NetworkManager D-Bus API
//! directly: the D-Bus surface needs per-property object walks and a nested
//! `a{sa{sv}}` settings dict just to activate a PSK network, where `nmcli`
//! needs one argv. `display.rs` and `audio.rs` set the same precedent.
//!
//! Arguments are passed as a real argv (never a shell string), and every
//! agent-supplied value is validated before it reaches that argv.

use std::process::Stdio;
use std::time::Duration;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;
use zeroize::Zeroizing;

use crate::hal::HalDriver;

const NMCLI: &str = "nmcli";
/// `nmcli`'s own default connect timeout is 90s and it can wedge behind a
/// stuck supplicant, so cap every call well under that.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ARG_LEN: usize = 128;

/// Split one line of `nmcli -t` terse output into fields.
///
/// Terse mode separates fields with `:` and escapes literal colons as `\:`
/// and backslashes as `\\`. BSSIDs are all colons and an SSID may contain
/// them too, so a naive `split(':')` would let a hostile SSID shift every
/// field to its right.
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(escaped) => current.push(escaped),
                // Trailing lone backslash: keep it rather than dropping data.
                None => current.push('\\'),
            },
            ':' => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

/// Reject agent-supplied values that would be re-interpreted by `nmcli`.
///
/// SSIDs and interface names land in positional argv slots. A value starting
/// with `-` is parsed by `nmcli` as an option, which is the one way an argv
/// (no-shell) call can still be steered by its input.
fn validate_arg(kind: &str, value: &str) -> Result<(), AgentOSError> {
    if value.is_empty() {
        return Err(AgentOSError::HalError(format!("{kind} must not be empty")));
    }
    if value.len() > MAX_ARG_LEN {
        return Err(AgentOSError::HalError(format!(
            "{kind} exceeds {MAX_ARG_LEN} bytes"
        )));
    }
    if value.starts_with('-') {
        return Err(AgentOSError::HalError(format!(
            "{kind} must not start with '-' (would be parsed as an nmcli option)"
        )));
    }
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(AgentOSError::HalError(format!(
            "{kind} must not contain NUL or newlines"
        )));
    }
    Ok(())
}

/// Validate a value that nmcli consumes in a *value* slot (immediately after
/// its keyword), where it can never be re-read as an option.
///
/// Deliberately does NOT apply the leading-`-` rule: `-Secret123!` is a valid
/// WPA passphrase, and nmcli's `next_arg` takes the token unconditionally.
fn validate_secret(kind: &str, value: &str) -> Result<(), AgentOSError> {
    if value.is_empty() {
        return Err(AgentOSError::HalError(format!("{kind} must not be empty")));
    }
    if value.len() > MAX_ARG_LEN {
        return Err(AgentOSError::HalError(format!(
            "{kind} exceeds {MAX_ARG_LEN} bytes"
        )));
    }
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(AgentOSError::HalError(format!(
            "{kind} must not contain NUL or newlines"
        )));
    }
    Ok(())
}

fn validate_ifname(value: &str) -> Result<(), AgentOSError> {
    validate_arg("ifname", value)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
    {
        return Err(AgentOSError::HalError(
            "ifname must be alphanumeric with '.', '_', '-' or ':'".to_string(),
        ));
    }
    Ok(())
}

/// Trimmed accessor — for values where surrounding whitespace is meaningless
/// (`action`, `ifname`).
/// Build the `nmcli device wifi connect` argv.
///
/// The SSID MUST stay at index 3. nmcli consumes that slot positionally before
/// entering its keyword loop, which is what makes an SSID literally named
/// `password` or `hidden` harmless. Reordering these pushes, or inserting a
/// value ahead of the SSID, silently breaks that guarantee — hence the test.
fn connect_args(ssid: &str, password: Option<&str>, ifname: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "device".to_string(),
        "wifi".to_string(),
        "connect".to_string(),
        ssid.to_string(),
    ];
    if let Some(secret) = password {
        args.push("password".to_string());
        args.push(secret.to_string());
    }
    if let Some(name) = ifname {
        args.push("ifname".to_string());
        args.push(name.to_string());
    }
    args
}

/// nmcli's "SSID is not in my scan cache" failure.
///
/// `nmcli device wifi connect` resolves the SSID against NetworkManager's
/// *cached* AP list, not a fresh sweep, so a weak or briefly-unseen AP fails
/// instantly while still in range. One forced rescan fixes it; every other
/// nmcli failure (bad PSK, radio off, no device) does not, so match narrowly.
///
/// Both halves are load-bearing. The message text is sound to match because
/// [`WifiDriver::run`] pins `LC_ALL=C`, so nmcli's own strings stay
/// untranslated. The exit code pins WHICH nmcli step failed
/// (`NMC_RESULT_ERROR_NOT_FOUND` is 10): an SSID literally named
/// `No network with SSID` is legal 802.11 and passes [`validate_arg`], and
/// nmcli echoes the profile name in unrelated errors, so the substring alone
/// lets a nearby AP name arm the retry.
fn is_ssid_not_in_scan_cache(error: &AgentOSError) -> bool {
    matches!(
        error,
        AgentOSError::HalError(message)
            if message.starts_with("nmcli exited with 10:")
                && message.contains("No network with SSID")
    )
}

fn param_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Verbatim accessor — for values where whitespace is significant.
///
/// A WPA passphrase is 8-63 printable ASCII characters, spaces included and
/// legal at either end; 802.11 SSIDs may likewise begin or end with a space.
/// Trimming them silently authenticates with the wrong credential or reports a
/// network name the caller never asked for.
fn param_str_raw<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub struct WifiDriver;

impl Default for WifiDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl WifiDriver {
    pub fn new() -> Self {
        Self
    }

    /// Run `nmcli` and return stdout.
    ///
    /// Args are deliberately never logged here: `connect` puts the PSK in this
    /// vector, and one unconditional rule beats an exception to remember.
    async fn run(&self, args: &[String]) -> Result<String, AgentOSError> {
        let spawned = Command::new(NMCLI)
            .args(args)
            // nmcli's state words go through gettext: under LANG=de_DE the
            // radio reports "aktiviert", not "enabled", and every parse below
            // silently reads the wrong thing.
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Without this a timed-out child is merely dropped, not killed: it
            // keeps running to nmcli's own 90s deadline with the PSK still in
            // its argv and stacks up against the next attempt on the same
            // interface.
            //
            // It does NOT make a timed-out connect atomic. nmcli is a D-Bus
            // client: once `AddAndActivateConnection` has returned,
            // NetworkManager finishes the association whether or not the client
            // is alive, so a caller told "timed out" may still end up joined,
            // with the auto-generated profile kept.
            .kill_on_drop(true)
            .output();

        let output = tokio::time::timeout(CALL_TIMEOUT, spawned)
            .await
            .map_err(|_| {
                AgentOSError::HalError(format!("nmcli timed out after {}s", CALL_TIMEOUT.as_secs()))
            })?
            .map_err(|error| {
                AgentOSError::HalError(format!(
                    "Failed to spawn '{NMCLI}': {error}. Is NetworkManager installed?"
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(AgentOSError::HalError(format!(
                "nmcli exited with {}: {stderr}",
                output.status.code().unwrap_or(-1)
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// `nmcli -t` rows, blank lines dropped.
    async fn run_terse(&self, args: &[&str]) -> Result<Vec<Vec<String>>, AgentOSError> {
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let stdout = self.run(&owned).await?;
        Ok(stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(split_terse)
            .collect())
    }

    async fn radio_enabled(&self) -> Result<bool, AgentOSError> {
        let rows = self.run_terse(&["-t", "-f", "WIFI", "radio"]).await?;
        Ok(rows
            .first()
            .and_then(|row| row.first())
            .map(|state| state.trim().eq_ignore_ascii_case("enabled"))
            .unwrap_or(false))
    }

    async fn status(&self) -> Result<Value, AgentOSError> {
        let radio_enabled = self.radio_enabled().await?;
        let rows = self
            .run_terse(&[
                "-t",
                "-f",
                "DEVICE,TYPE,STATE,CONNECTION",
                "device",
                "status",
            ])
            .await?;

        let (well_formed, malformed): (Vec<_>, Vec<_>) =
            rows.iter().partition(|row| row.len() >= 4);
        let devices: Vec<Value> = well_formed
            .iter()
            .filter(|row| row[1] == "wifi")
            .map(|row| {
                json!({
                    "device": row[0],
                    "state": row[2],
                    "connection": (!row[3].is_empty()).then(|| row[3].clone()),
                })
            })
            .collect();

        Ok(json!({
            "action": "status",
            "radio_enabled": radio_enabled,
            "devices": devices,
            // nmcli does not escape control characters, so an SSID containing a
            // raw newline splits its record across lines. Report the count
            // rather than letting a connected interface silently disappear.
            "unparsed_rows": malformed.len(),
        }))
    }

    async fn scan(&self, params: &Value) -> Result<Value, AgentOSError> {
        let mut args = vec![
            "-t",
            "-f",
            "IN-USE,SSID,BSSID,SIGNAL,SECURITY,FREQ",
            "device",
            "wifi",
            "list",
        ];

        let ifname = param_str(params, "ifname");
        if let Some(name) = ifname {
            validate_ifname(name)?;
            args.push("ifname");
            args.push(name);
        }

        // `--rescan yes` forces a fresh sweep instead of returning NM's cache.
        args.push("--rescan");
        args.push("yes");

        let rows = self.run_terse(&args).await?;
        let (well_formed, malformed): (Vec<_>, Vec<_>) =
            rows.iter().partition(|row| row.len() >= 6);
        let networks: Vec<Value> = well_formed
            .iter()
            .map(|row| {
                json!({
                    "in_use": row[0].trim() == "*",
                    "ssid": row[1],
                    "bssid": row[2],
                    "signal": row[3].parse::<u8>().ok(),
                    "security": (!row[4].is_empty()).then(|| row[4].clone()),
                    "frequency": row[5],
                })
            })
            .collect();

        Ok(json!({
            "action": "scan",
            "count": networks.len(),
            "networks": networks,
            // A rogue AP can broadcast an SSID with a raw newline to split its
            // own row and hide from the scan. Surface it as an anomaly rather
            // than an absence.
            "unparsed_rows": malformed.len(),
        }))
    }

    async fn connect(&self, params: &Value) -> Result<Value, AgentOSError> {
        // Verbatim, not trimmed: an SSID may legally begin or end with a space.
        let ssid = param_str_raw(params, "ssid").ok_or_else(|| {
            AgentOSError::HalError("connect requires an 'ssid' parameter".to_string())
        })?;
        validate_arg("ssid", ssid)?;

        // ponytail: the PSK rides in argv, so it is readable in
        // /proc/<pid>/cmdline for the length of the call. `nmcli device wifi
        // connect` has no `passwd-file` option (only `connection up` does);
        // upgrade to `connection add` + `connection up --passwd-file` if
        // AgentOS ever runs on a multi-user host. Note this is only the LAST
        // plaintext copy: the payload `Value`, the hook `input_json`, and the
        // argv `Vec<String>` all hold the secret un-zeroized too.
        let password = param_str_raw(params, "password").map(|p| Zeroizing::new(p.to_string()));
        if let Some(secret) = password.as_ref() {
            validate_secret("password", secret)?;
        }

        let ifname = param_str(params, "ifname");
        if let Some(name) = ifname {
            validate_ifname(name)?;
        }

        let args = connect_args(ssid, password.as_ref().map(|s| s.as_str()), ifname);

        let mut result = self.run(&args).await;
        // A cache miss is indistinguishable from an absent network to the
        // caller, and the agent above reads it as "out of range" — it asks for
        // a password it does not need, or gives up on an AP it just listed.
        // Re-sweep once and retry.
        //
        // Retrying a state-changing call is safe here specifically because
        // nmcli resolves the SSID against the AP list BEFORE it calls
        // `AddAndActivateConnection`: this failure means no activation was ever
        // requested, so the second attempt cannot double-join.
        //
        // `scan` is reused rather than hand-rolling `--rescan yes` so the flag
        // and the `ifname` validation stay defined in one place. Note it runs
        // driver-local, so `hardware.wifi.scan:o` is NOT checked for it —
        // `hardware.wifi.connection:x` alone can now trigger a radio sweep.
        let mut rescan_exhausted = false;
        if result.as_ref().is_err_and(is_ssid_not_in_scan_cache) {
            // NM refuses an explicit rescan while a device is activating or
            // right after a previous sweep. The retry is still worth making
            // (a sibling's in-flight scan refreshes the cache too), but a
            // refusal is the one way this fix silently does nothing, so say so.
            if let Err(error) = self.scan(params).await {
                tracing::debug!(%error, "forced rescan before wifi connect retry failed");
            }
            result = self.run(&args).await;
            rescan_exhausted = result.as_ref().is_err_and(is_ssid_not_in_scan_cache);
        }
        // nmcli echoes the profile name on failure, not the secret, but redact
        // defensively rather than trusting that across nmcli versions.
        let stdout = match result {
            Ok(stdout) => stdout,
            Err(AgentOSError::HalError(message)) => {
                let message = match password.as_ref() {
                    Some(secret) => message.replace(secret.as_str(), "***"),
                    None => message,
                };
                // Without this the retried failure is byte-identical to the
                // first, so the agent above runs the same scan/ask/retry loop
                // this fix exists to break — now paying a full sweep per lap.
                return Err(AgentOSError::HalError(if rescan_exhausted {
                    format!(
                        "{message} A forced rescan did not find it either — \
                         the AP is not currently broadcasting (a hidden SSID \
                         also reports this)."
                    )
                } else {
                    message
                }));
            }
            Err(other) => return Err(other),
        };

        Ok(json!({
            "action": "connect",
            "ssid": ssid,
            "detail": stdout.trim(),
        }))
    }

    async fn disconnect(&self, params: &Value) -> Result<Value, AgentOSError> {
        let ifname = param_str(params, "ifname").ok_or_else(|| {
            AgentOSError::HalError("disconnect requires an 'ifname' parameter".to_string())
        })?;
        validate_ifname(ifname)?;

        let stdout = self
            .run(&[
                "device".to_string(),
                "disconnect".to_string(),
                ifname.to_string(),
            ])
            .await?;

        Ok(json!({
            "action": "disconnect",
            "ifname": ifname,
            "detail": stdout.trim(),
        }))
    }

    async fn radio(&self, params: &Value) -> Result<Value, AgentOSError> {
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                AgentOSError::HalError("radio requires a boolean 'enabled' parameter".to_string())
            })?;

        self.run(&[
            "radio".to_string(),
            "wifi".to_string(),
            if enabled { "on" } else { "off" }.to_string(),
        ])
        .await?;

        Ok(json!({
            "action": "radio",
            "radio_enabled": enabled,
        }))
    }
}

#[async_trait]
impl HalDriver for WifiDriver {
    fn name(&self) -> &str {
        "wifi"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.wifi.list", PermissionOp::Read)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
        {
            "status" => ("hardware.wifi.list", PermissionOp::Read),
            "scan" => ("hardware.wifi.scan", PermissionOp::Observe),
            "connect" | "disconnect" => ("hardware.wifi.connection", PermissionOp::Execute),
            "radio" => ("hardware.wifi.radio", PermissionOp::Execute),
            // Unknown actions are rejected outright by `query`; map them to the
            // narrowest permission so an unknown string can never widen access.
            _ => ("hardware.wifi.list", PermissionOp::Read),
        }
    }

    /// Key state-changing actions to a device so the `DeviceAccessGate` and
    /// hardware quarantine engage, the way `bluetooth`/`webcam`/`usb_storage`
    /// do. Read-only actions return `None` and stay ungated.
    ///
    /// `connect` keys on the SSID: joining an arbitrary access point is the
    /// direct analogue of pairing with an untrusted Bluetooth peer, and the
    /// `ifname` is optional there so it cannot be the key.
    fn device_key(&self, params: &Value) -> Option<String> {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
        {
            "connect" => param_str_raw(params, "ssid")
                .filter(|ssid| validate_arg("ssid", ssid).is_ok())
                .map(|ssid| format!("wifi:ssid:{ssid}")),
            "disconnect" => param_str(params, "ifname")
                .filter(|name| validate_ifname(name).is_ok())
                .map(|name| format!("wifi:{name}")),
            // `radio()` runs the global `nmcli radio wifi on|off` and never
            // reads `ifname`, so keying on one left every radio toggle with a
            // `None` key — i.e. outside the per-device approval gate. Key on
            // the global switch it actually flips.
            "radio" => Some("wifi:radio".to_string()),
            _ => None,
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status");

        match action {
            "status" => self.status().await,
            "scan" => self.scan(&params).await,
            "connect" => self.connect(&params).await,
            "disconnect" => self.disconnect(&params).await,
            "radio" => self.radio(&params).await,
            other => Err(AgentOSError::HalError(format!(
                "Unknown wifi action '{other}' (expected status, scan, connect, disconnect, radio)"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_terse_handles_plain_fields() {
        assert_eq!(
            split_terse("wlan0:wifi:connected:home"),
            vec!["wlan0", "wifi", "connected", "home"]
        );
    }

    #[test]
    fn split_terse_unescapes_colons_in_bssids() {
        // `nmcli -t` renders a BSSID's colons as `\:`.
        let row = split_terse("*:MyNet:AA\\:BB\\:CC\\:DD\\:EE\\:FF:72:WPA2:2437 MHz");
        assert_eq!(row.len(), 6);
        assert_eq!(row[1], "MyNet");
        assert_eq!(row[2], "AA:BB:CC:DD:EE:FF");
        assert_eq!(row[3], "72");
        assert_eq!(row[4], "WPA2");
    }

    #[test]
    fn split_terse_keeps_hostile_ssid_in_one_field() {
        // An SSID containing colons must not shift the fields to its right.
        let row = split_terse("*:evil\\:net\\:2:AA\\:BB:50:WPA2:5180 MHz");
        assert_eq!(row.len(), 6);
        assert_eq!(row[1], "evil:net:2");
        assert_eq!(row[3], "50");
    }

    #[test]
    fn split_terse_preserves_empty_trailing_field() {
        let row = split_terse("wlan0:wifi:disconnected:");
        assert_eq!(row.len(), 4);
        assert_eq!(row[3], "");
    }

    #[test]
    fn validate_arg_rejects_option_lookalikes() {
        assert!(validate_arg("ssid", "-t").is_err());
        assert!(validate_arg("ssid", "--terse").is_err());
        assert!(validate_arg("ssid", "").is_err());
        assert!(validate_arg("ssid", "a\nb").is_err());
        assert!(validate_arg("ssid", &"x".repeat(MAX_ARG_LEN + 1)).is_err());
        assert!(validate_arg("ssid", "My Network 5G").is_ok());
    }

    #[test]
    fn validate_ifname_rejects_shell_and_path_characters() {
        assert!(validate_ifname("wlan0").is_ok());
        assert!(validate_ifname("wlp3s0").is_ok());
        assert!(validate_ifname("wlan0 ; rm -rf /").is_err());
        assert!(validate_ifname("../etc").is_err());
        assert!(validate_ifname("-x").is_err());
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let driver = WifiDriver::new();
        let error = driver
            .query(json!({ "action": "exfiltrate" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Unknown wifi action"));
    }

    #[tokio::test]
    async fn connect_requires_ssid_before_spawning() {
        let driver = WifiDriver::new();
        let error = driver
            .query(json!({ "action": "connect" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("requires an 'ssid'"));
    }

    #[tokio::test]
    async fn connect_rejects_option_lookalike_ssid() {
        let driver = WifiDriver::new();
        let error = driver
            .query(json!({ "action": "connect", "ssid": "--terse" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("must not start with '-'"));
    }

    #[tokio::test]
    async fn radio_requires_enabled_flag() {
        let driver = WifiDriver::new();
        let error = driver
            .query(json!({ "action": "radio" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("boolean 'enabled'"));
    }

    /// The entire no-injection argument rests on the SSID staying positional
    /// at index 3, ahead of every keyword. Assert the shape, not just the
    /// rejections — a future param inserted before it breaks this silently.
    #[test]
    fn ssid_stays_positional_ahead_of_every_keyword() {
        let args = connect_args("password", Some("pw"), Some("wlan0"));
        assert_eq!(
            args,
            vec!["device", "wifi", "connect", "password", "password", "pw", "ifname", "wlan0"]
        );
        // Index 3 is the positional SSID slot, even when the SSID is spelled
        // exactly like an nmcli keyword.
        assert_eq!(args[3], "password");
    }

    #[test]
    fn connect_args_omit_absent_optionals() {
        assert_eq!(
            connect_args("HomeNet", None, None),
            vec!["device", "wifi", "connect", "HomeNet"]
        );
        assert_eq!(
            connect_args("HomeNet", None, Some("wlan0")),
            vec!["device", "wifi", "connect", "HomeNet", "ifname", "wlan0"]
        );
    }

    /// The observed failure (OSS chat session `e5abad53`, msg 640) must arm the
    /// rescan retry, and nothing else may — a wrong PSK retried after a 25s
    /// sweep just re-attempts authentication.
    #[test]
    fn only_a_scan_cache_miss_arms_the_retry() {
        let miss = AgentOSError::HalError(
            "nmcli exited with 10: Error: No network with SSID 'GNXS-5G-3A62C0' found.".to_string(),
        );
        assert!(is_ssid_not_in_scan_cache(&miss));

        for other in [
            "nmcli exited with 4: Error: Connection activation failed: (7) Secrets were required, but not provided.",
            "nmcli exited with 8: Error: NetworkManager is not running.",
            "nmcli timed out after 60s",
            // An 802.11 SSID may legally be named after the sentinel, and
            // nmcli echoes the profile name in unrelated errors. The exit code
            // is what keeps a nearby AP from arming a rescan on every failure.
            "nmcli exited with 4: Error: Connection activation failed for 'No network with SSID'.",
        ] {
            assert!(
                !is_ssid_not_in_scan_cache(&AgentOSError::HalError(other.to_string())),
                "must not retry: {other}"
            );
        }
    }

    #[test]
    fn split_terse_unescapes_backslashes() {
        // The other half of nmcli's escaping contract: `\\` -> `\`.
        let row = split_terse("a\\\\b:c");
        assert_eq!(row, vec!["a\\b", "c"]);
    }

    /// A WPA passphrase may legally start, end, or consist of spaces, and an
    /// SSID may too. Trimming either corrupts the credential or misreports the
    /// network name.
    #[test]
    fn whitespace_significant_params_are_not_trimmed() {
        let params = json!({ "ssid": " Guest ", "password": " pass word " });
        assert_eq!(param_str_raw(&params, "ssid"), Some(" Guest "));
        assert_eq!(param_str_raw(&params, "password"), Some(" pass word "));
        // ifname keeps the trimming: whitespace is meaningless there.
        let iface = json!({ "ifname": " wlan0 " });
        assert_eq!(param_str(&iface, "ifname"), Some("wlan0"));
    }

    /// `-Secret123!` is a valid PSK. It sits in a value slot nmcli consumes
    /// with `next_arg`, so the leading-`-` rule must not apply to it.
    #[test]
    fn validate_secret_allows_leading_dash_but_still_blocks_control_chars() {
        assert!(validate_secret("password", "-Secret123!").is_ok());
        assert!(validate_secret("password", "  pad  ").is_ok());
        assert!(validate_secret("password", "a\nb").is_err());
        assert!(validate_secret("password", "").is_err());
        assert!(validate_secret("password", &"x".repeat(MAX_ARG_LEN + 1)).is_err());
    }

    #[test]
    fn device_key_gates_state_changing_actions_only() {
        let driver = WifiDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "action": "connect", "ssid": "HomeNet" })),
            Some("wifi:ssid:HomeNet".to_string())
        );
        assert_eq!(
            driver.device_key(&json!({ "action": "disconnect", "ifname": "wlan0" })),
            Some("wifi:wlan0".to_string())
        );
        assert_eq!(driver.device_key(&json!({ "action": "status" })), None);
        assert_eq!(driver.device_key(&json!({ "action": "scan" })), None);
        // `radio` flips the global switch and ignores `ifname`, so it must be
        // gated with or without one — keying on `ifname` left it ungated.
        assert_eq!(
            driver.device_key(&json!({ "action": "radio", "enabled": false })),
            Some("wifi:radio".to_string())
        );
        assert_eq!(
            driver.device_key(&json!({ "action": "radio", "ifname": "wlan0", "enabled": true })),
            Some("wifi:radio".to_string())
        );
    }

    #[test]
    fn permission_mapping_is_per_action() {
        let driver = WifiDriver::new();
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "scan" })),
            ("hardware.wifi.scan", PermissionOp::Observe)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "connect" })),
            ("hardware.wifi.connection", PermissionOp::Execute)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "radio" })),
            ("hardware.wifi.radio", PermissionOp::Execute)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "status" })),
            ("hardware.wifi.list", PermissionOp::Read)
        );
    }
}
