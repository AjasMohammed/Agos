//! Boot-time host probes for the peripheral drivers.
//!
//! A peripheral driver compiled into the binary is only useful if the host has
//! the hardware *and* the service the driver talks to. The kernel registers a
//! compiled-in peripheral driver only when its probe passes (or the operator
//! forces it), and `agentos doctor` prints the same table.
//!
//! Probes are filesystem and environment lookups only. They never spawn a
//! process or open a D-Bus connection, so they cannot hang boot.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The peripheral driver names probed here, identical to `HalDriver::name()`.
pub const PERIPHERAL_DRIVERS: &[&str] = &[
    "audio",
    "bluetooth",
    "display",
    "printer",
    "raw-usb",
    "usb-storage",
    "webcam",
    "wifi",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeripheralProbe {
    /// HAL driver name, identical to `HalDriver::name()`.
    pub driver: &'static str,
    pub present: bool,
    /// Why the probe passed or failed, for logs and `agentos doctor`.
    pub reason: String,
}

/// Inputs for probing. [`ProbeEnv::from_host`] for real use; tests build one
/// by hand with `root` pointing at a tempdir.
#[derive(Debug, Clone, Default)]
pub struct ProbeEnv {
    /// Filesystem root; `/` on a real host.
    pub root: PathBuf,
    pub path: Option<OsString>,
    pub xdg_runtime_dir: Option<PathBuf>,
    pub wayland_display: Option<OsString>,
    pub display: Option<OsString>,
}

impl ProbeEnv {
    pub fn from_host() -> Self {
        Self {
            root: PathBuf::from("/"),
            path: std::env::var_os("PATH"),
            xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            display: std::env::var_os("DISPLAY"),
        }
    }

    /// `rel` must be relative: `Path::join` with an absolute path drops `root`.
    fn at(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn on_path(&self, bin: &str) -> bool {
        self.path
            .as_ref()
            .is_some_and(|p| std::env::split_paths(p).any(|d| d.join(bin).is_file()))
    }

    fn any_entry(&self, dir: &str, pred: impl Fn(&str, &Path) -> bool) -> bool {
        std::fs::read_dir(self.at(dir)).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                let name = e.file_name();
                pred(&name.to_string_lossy(), &e.path())
            })
        })
    }
}

fn set(v: &Option<OsString>) -> bool {
    v.as_ref().is_some_and(|s| !s.is_empty())
}

/// Probe every peripheral driver against the real host.
pub fn probe_peripherals() -> Vec<PeripheralProbe> {
    probe_peripherals_with(&ProbeEnv::from_host())
}

/// Probe every peripheral driver, regardless of which features are compiled
/// in. The caller decides what to do with the result.
pub fn probe_peripherals_with(env: &ProbeEnv) -> Vec<PeripheralProbe> {
    PERIPHERAL_DRIVERS
        .iter()
        .map(|&driver| {
            let (present, reason) = match probe_one(env, driver) {
                Ok(why) => (true, why),
                Err(why) => (false, why),
            };
            PeripheralProbe {
                driver,
                present,
                reason,
            }
        })
        .collect()
}

/// `Ok(why present)` or `Err(why absent)`. Each check mirrors what the driver
/// actually calls, not just the hardware.
fn probe_one(env: &ProbeEnv, driver: &str) -> Result<String, String> {
    match driver {
        // audio.rs drives PipeWire only (wpctl, pw-play, pw-record, pw-cli).
        "audio" => {
            let cards = std::fs::read_to_string(env.at("proc/asound/cards")).unwrap_or_default();
            if cards.trim().is_empty() || cards.contains("no soundcards") {
                return Err("no sound card in /proc/asound/cards".into());
            }
            if !env.on_path("wpctl") {
                return Err("wpctl not on PATH (PipeWire tools)".into());
            }
            match &env.xdg_runtime_dir {
                Some(dir) if dir.join("pipewire-0").exists() => Ok("sound card + PipeWire".into()),
                _ => Err("no PipeWire socket ($XDG_RUNTIME_DIR/pipewire-0)".into()),
            }
        }
        // bluetooth.rs talks to BlueZ about a local adapter.
        "bluetooth" => env
            .any_entry("sys/class/bluetooth", |n, _| n.starts_with("hci"))
            .then(|| "adapter in /sys/class/bluetooth".to_string())
            .ok_or_else(|| "no adapter in /sys/class/bluetooth".into()),
        // display.rs uses wlr-randr / xrandr in a session, else reads DRM
        // connectors from sysfs (list only).
        "display" => {
            if set(&env.wayland_display) && env.on_path("wlr-randr") {
                Ok("Wayland session + wlr-randr".into())
            } else if set(&env.display) && env.on_path("xrandr") {
                Ok("X session + xrandr".into())
            } else if env.any_entry("sys/class/drm", |n, _| {
                n.starts_with("card") && n.contains('-')
            }) {
                Ok("DRM connectors in /sys/class/drm (read-only)".into())
            } else {
                Err("no display session or DRM connectors".into())
            }
        }
        // printer.rs defaults to CUPS at ipp://localhost:631/.
        "printer" => ["run/cups/cups.sock", "var/run/cups/cups.sock"]
            .iter()
            .any(|p| env.at(p).exists())
            .then(|| "CUPS socket".to_string())
            .ok_or_else(|| "no local CUPS socket (use [hal] force_enable for remote CUPS)".into()),
        // raw_usb.rs opens USB devices directly.
        "raw-usb" => usb_devices(env)
            .then(|| "USB devices present".to_string())
            .ok_or_else(|| "no USB devices in /sys/bus/usb/devices".into()),
        // usb_storage.rs talks to org.freedesktop.UDisks2 on the system bus.
        "usb-storage" => {
            if !usb_devices(env) {
                Err("no USB devices in /sys/bus/usb/devices".into())
            } else if !env
                .at("usr/share/dbus-1/system-services/org.freedesktop.UDisks2.service")
                .exists()
            {
                Err("UDisks2 not installed".into())
            } else {
                Ok("USB + UDisks2".into())
            }
        }
        // webcam.rs opens V4L2 capture devices.
        "webcam" => env
            .any_entry("sys/class/video4linux", |n, _| n.starts_with("video"))
            .then(|| "V4L2 device present".to_string())
            .ok_or_else(|| "no /sys/class/video4linux/video*".into()),
        // wifi.rs drives NetworkManager through nmcli.
        "wifi" => {
            if !env.any_entry("sys/class/net", |_, p| p.join("wireless").is_dir()) {
                Err("no wireless interface in /sys/class/net".into())
            } else if !env.on_path("nmcli") {
                Err("nmcli not on PATH (NetworkManager)".into())
            } else {
                Ok("wireless interface + nmcli".into())
            }
        }
        other => Err(format!("unknown peripheral driver '{other}'")),
    }
}

fn usb_devices(env: &ProbeEnv) -> bool {
    env.any_entry("sys/bus/usb/devices", |_, _| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Host {
        _dir: tempfile::TempDir,
        env: ProbeEnv,
    }

    impl Host {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("bin");
            fs::create_dir_all(&bin).unwrap();
            let env = ProbeEnv {
                root: dir.path().to_path_buf(),
                path: Some(bin.into_os_string()),
                ..Default::default()
            };
            Self { _dir: dir, env }
        }
        fn mkdir(&self, rel: &str) {
            fs::create_dir_all(self.env.at(rel)).unwrap();
        }
        fn file(&self, rel: &str, body: &str) {
            let p = self.env.at(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        fn bin(&self, name: &str) {
            self.file(&format!("bin/{name}"), "");
        }
        fn present(&self, driver: &str) -> bool {
            probe_peripherals_with(&self.env)
                .into_iter()
                .find(|p| p.driver == driver)
                .unwrap()
                .present
        }
    }

    #[test]
    fn empty_host_has_no_peripherals() {
        let host = Host::new();
        let probes = probe_peripherals_with(&host.env);
        assert_eq!(probes.len(), PERIPHERAL_DRIVERS.len());
        for p in probes {
            assert!(!p.present, "{} should be absent", p.driver);
            assert!(!p.reason.is_empty());
        }
    }

    #[test]
    fn wifi_needs_wireless_iface_and_nmcli() {
        let host = Host::new();
        host.mkdir("sys/class/net/wlan0/wireless");
        assert!(!host.present("wifi"));
        host.bin("nmcli");
        assert!(host.present("wifi"));
    }

    #[test]
    fn audio_needs_card_wpctl_and_pipewire_socket() {
        let mut host = Host::new();
        host.bin("wpctl");
        host.file("run/user/1000/pipewire-0", "");
        host.env.xdg_runtime_dir = Some(host.env.at("run/user/1000"));
        host.file("proc/asound/cards", " --- no soundcards ---\n");
        assert!(!host.present("audio"));
        host.file(
            "proc/asound/cards",
            " 0 [PCH ]: HDA-Intel - HDA Intel PCH\n",
        );
        assert!(host.present("audio"));
        host.env.xdg_runtime_dir = None;
        assert!(!host.present("audio"));
    }

    #[test]
    fn display_session_or_drm() {
        let mut host = Host::new();
        host.bin("xrandr");
        host.env.display = Some("".into());
        assert!(!host.present("display"));
        host.env.display = Some(":0".into());
        assert!(host.present("display"));
        host.env.display = None;
        host.mkdir("sys/class/drm/card0");
        assert!(!host.present("display"), "bare card0 is not a connector");
        host.mkdir("sys/class/drm/card0-HDMI-A-1");
        assert!(host.present("display"));
    }

    #[test]
    fn printer_via_cups_socket() {
        let host = Host::new();
        host.file("var/run/cups/cups.sock", "");
        assert!(host.present("printer"));
    }

    #[test]
    fn usb_storage_needs_udisks2() {
        let host = Host::new();
        host.mkdir("sys/bus/usb/devices/1-1");
        assert!(host.present("raw-usb"));
        assert!(!host.present("usb-storage"));
        host.file(
            "usr/share/dbus-1/system-services/org.freedesktop.UDisks2.service",
            "",
        );
        assert!(host.present("usb-storage"));
    }

    #[test]
    fn bluetooth_and_webcam_from_sysfs() {
        let host = Host::new();
        host.mkdir("sys/class/bluetooth/hci0");
        host.mkdir("sys/class/video4linux/video0");
        assert!(host.present("bluetooth"));
        assert!(host.present("webcam"));
    }
}
