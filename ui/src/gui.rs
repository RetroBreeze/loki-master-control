use gtk::cairo;
use gtk::gdk;
use gtk::glib::{self, SourceId};
use gtk::prelude::*;
use gtk::{Align, Application, ApplicationWindow, Orientation};
use gtk4 as gtk;
use gtk4_layer_shell::{self as layer_shell, LayerShell};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::fs;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    runtime::Runtime,
};

static BRIGHTNESS_PATH: OnceLock<String> = OnceLock::new();
static MAX_BRIGHTNESS: OnceLock<u32> = OnceLock::new();
static DEFAULT_SINK: OnceLock<String> = OnceLock::new();
static PWM_BASE: OnceLock<Option<String>> = OnceLock::new();

#[derive(Clone, Copy)]
struct FanPoint {
    temp: f32,
    percent: f32,
}

static QUIET_CURVE: [FanPoint; 5] = [
    FanPoint {
        temp: 40.0,
        percent: 0.0,
    },
    FanPoint {
        temp: 50.0,
        percent: 20.0,
    },
    FanPoint {
        temp: 60.0,
        percent: 40.0,
    },
    FanPoint {
        temp: 70.0,
        percent: 70.0,
    },
    FanPoint {
        temp: 80.0,
        percent: 100.0,
    },
];

static AGGRESSIVE_CURVE: [FanPoint; 5] = [
    FanPoint {
        temp: 30.0,
        percent: 20.0,
    },
    FanPoint {
        temp: 40.0,
        percent: 40.0,
    },
    FanPoint {
        temp: 50.0,
        percent: 60.0,
    },
    FanPoint {
        temp: 60.0,
        percent: 80.0,
    },
    FanPoint {
        temp: 70.0,
        percent: 100.0,
    },
];

#[derive(Clone, Debug)]
struct DisplayMode {
    width: u32,
    height: u32,
    refresh: u32,
}

fn query_display_modes() -> Option<(String, Vec<DisplayMode>)> {
    let output = Command::new("wlr-randr").arg("--json").output().ok()?;
    let val: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let connectors = val.get("connectors")?.as_array()?;
    let conn = connectors.iter().find(|c| c.get("modes").is_some())?;
    let name = conn.get("name")?.as_str()?.to_string();
    let modes_arr = conn.get("modes")?.as_array()?;
    let mut modes = Vec::new();
    for m in modes_arr {
        let w = m.get("width").and_then(|v| v.as_u64())? as u32;
        let h = m.get("height").and_then(|v| v.as_u64())? as u32;
        let r = m.get("refresh").and_then(|v| v.as_u64())? as u32;
        modes.push(DisplayMode {
            width: w,
            height: h,
            refresh: r,
        });
    }
    Some((name, modes))
}

fn apply_display_mode(output: &str, width: u32, height: u32, refresh: u32) {
    let hz = refresh as f64 / 1000.0;
    let mode = format!("{}x{}@{}Hz", width, height, hz);
    daemon_send(json!({
        "cmd": "run",
        "program": "wlr-randr",
        "args": ["--output", output, "--mode", mode]
    }));
}

fn init_backlight() {
    if MAX_BRIGHTNESS.get().is_some() && BRIGHTNESS_PATH.get().is_some() {
        return;
    }
    let dir_iter = match fs::read_dir("/sys/class/backlight") {
        Ok(it) => it,
        Err(e) => {
            eprintln!("Failed to read /sys/class/backlight: {}", e);
            return;
        }
    };
    let entry = match dir_iter.into_iter().next() {
        Some(Ok(e)) => e.path(),
        Some(Err(e)) => {
            eprintln!("Error reading backlight entry: {}", e);
            return;
        }
        None => {
            eprintln!("No backlight device found");
            return;
        }
    };

    let max_path = entry.join("max_brightness");
    match fs::read_to_string(&max_path) {
        Ok(s) => match s.trim().parse::<u32>() {
            Ok(v) => {
                let _ = MAX_BRIGHTNESS.set(v);
                let _ =
                    BRIGHTNESS_PATH.set(entry.join("brightness").to_string_lossy().into_owned());
            }
            Err(e) => eprintln!("Failed to parse {}: {}", max_path.display(), e),
        },
        Err(e) => eprintln!("Failed to read {}: {}", max_path.display(), e),
    }
}

fn read_max_brightness() -> u32 {
    init_backlight();
    MAX_BRIGHTNESS.get().copied().unwrap_or(100)
}

fn write_brightness(value: u32) {
    init_backlight();
    if let Some(path) = BRIGHTNESS_PATH.get() {
        daemon_send(json!({"cmd":"write","path":path,"value":value.to_string()}));
    } else {
        eprintln!("Backlight brightness path unavailable");
    }
}

fn default_sink() -> &'static str {
    DEFAULT_SINK
        .get_or_init(|| match Command::new("pactl").arg("info").output() {
            Ok(out) => {
                if let Ok(text) = String::from_utf8(out.stdout) {
                    for line in text.lines() {
                        if let Some(rest) = line.strip_prefix("Default Sink:") {
                            return rest.trim().to_string();
                        }
                    }
                }
                "@DEFAULT_SINK@".to_string()
            }
            Err(e) => {
                eprintln!("Failed to run pactl info: {}", e);
                "@DEFAULT_SINK@".to_string()
            }
        })
        .as_str()
}

fn rfkill_blocked(kind: &str) -> Option<bool> {
    let out = Command::new("rfkill").args(&["list", kind]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("Soft blocked:") {
            return Some(rest.trim() == "yes");
        }
    }
    None
}

fn find_aynec_hwmon() -> Option<String> {
    eprintln!("Scanning /sys/class/hwmon for aynec...");
    let dir_iter = match fs::read_dir("/sys/class/hwmon") {
        Ok(it) => it,
        Err(e) => {
            eprintln!("Failed to read /sys/class/hwmon: {}", e);
            return None;
        }
    };

    for entry in dir_iter.flatten() {
        let base = entry.path();
        match fs::read_to_string(base.join("name")) {
            Ok(name) => {
                let trimmed = name.trim();
                eprintln!(" - {} -> {}", base.display(), trimmed);
                if trimmed == "aynec" {
                    let path = base.to_string_lossy().into_owned();
                    eprintln!("Found aynec hwmon at {}", path);
                    return Some(path);
                }
            }
            Err(e) => {
                eprintln!("Failed to read {}/name: {}", base.display(), e);
            }
        }
    }

    eprintln!("aynec hwmon device not found");
    None
}

fn write_to_sysfs(path: &str, value: impl AsRef<str>) {
    daemon_send(json!({"cmd":"write","path":path,"value":value.as_ref()}));
}

const RGB_BASE: &str = "/sys/class/leds/ayn:rgb:joystick_rings";

fn rgb_set_mode(mode: u8) {
    write_to_sysfs(&format!("{}/led_mode", RGB_BASE), mode.to_string());
}

fn rgb_set_brightness(val: u8) {
    write_to_sysfs(&format!("{}/brightness", RGB_BASE), val.to_string());
}

fn rgb_set_intensity(r: u8, g: u8, b: u8) {
    write_to_sysfs(
        &format!("{}/multi_intensity", RGB_BASE),
        format!("{} {} {}", r, g, b),
    );
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let c = v * s;
    let hh = (h / 60.0) % 6.0;
    let x = c * (1.0 - ((hh % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match hh as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let r = ((r1 + m) * 255.0).round() as u8;
    let g = ((g1 + m) * 255.0).round() as u8;
    let b = ((b1 + m) * 255.0).round() as u8;
    (r, g, b)
}

fn pwm_base() -> Option<&'static str> {
    PWM_BASE.get_or_init(find_aynec_hwmon);
    let base = PWM_BASE.get().and_then(|o| o.as_deref());
    if let Some(b) = base {
        eprintln!("Using hwmon base {}", b);
    }
    base
}

fn read_temp(base: &str) -> Option<f32> {
    for idx in 1..=5 {
        let path = format!("{}/temp{}_input", base, idx);
        if let Ok(s) = fs::read_to_string(&path) {
            if let Ok(v) = s.trim().parse::<f32>() {
                return Some(v / 1000.0);
            }
        }
    }
    None
}

fn eval_curve(curve: &[FanPoint; 5], temp: f32) -> f32 {
    if temp <= curve[0].temp {
        return curve[0].percent;
    }
    for i in 0..curve.len() - 1 {
        if temp <= curve[i + 1].temp {
            let (t0, p0) = (curve[i].temp, curve[i].percent);
            let (t1, p1) = (curve[i + 1].temp, curve[i + 1].percent);
            let ratio = (temp - t0) / (t1 - t0);
            return p0 + ratio * (p1 - p0);
        }
    }
    curve[curve.len() - 1].percent
}

fn build_ui(app: &Application) {
    // Main window setup
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Loki Control Center")
        .build();

    window.init_layer_shell();
    window.set_layer(layer_shell::Layer::Overlay);
    window.set_anchor(layer_shell::Edge::Top, true);
    window.set_anchor(layer_shell::Edge::Bottom, true);
    window.set_anchor(layer_shell::Edge::Right, true);
    window.set_exclusive_zone(0);

    // Size to 30% of screen width
    if let Some(display) = gdk::Display::default() {
        if let Some(obj) = display.monitors().item(0) {
            if let Some(mon) = obj.downcast_ref::<gdk::Monitor>() {
                let geo = mon.geometry();
                let w = ((geo.width() as f32) * 0.3) as i32;
                window.set_default_size(w, geo.height());
            }
        }
    }

    // Dark theme
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }

    // Scrollable container
    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let vbox = gtk::Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    // Row 1: Connectivity buttons centered
    let row1 = gtk::Box::new(Orientation::Horizontal, 8);
    row1.set_halign(Align::Center);

    // Wi-Fi toggle
    let wifi_btn = gtk::ToggleButton::with_label("Wi-Fi");
    wifi_btn.add_css_class("circular");
    if let Some(blocked) = rfkill_blocked("wifi") {
        wifi_btn.set_active(blocked);
    }
    {
        wifi_btn.connect_toggled(|_| {
            daemon_send(json!({"cmd":"run","program":"rfkill","args":["toggle","wifi"]}));
        });
    }
    row1.append(&wifi_btn);

    // Bluetooth toggle
    let bt_btn = gtk::ToggleButton::with_label("Bluetooth");
    bt_btn.add_css_class("circular");
    if let Some(blocked) = rfkill_blocked("bluetooth") {
        bt_btn.set_active(blocked);
    }
    {
        bt_btn.connect_toggled(|_| {
            daemon_send(json!({"cmd":"run","program":"rfkill","args":["toggle","bluetooth"]}));
        });
    }
    row1.append(&bt_btn);

    // Airplane mode toggle
    let airplane_btn = gtk::ToggleButton::with_label("Airplane");
    airplane_btn.add_css_class("circular");
    if rfkill_blocked("wifi") == Some(true) && rfkill_blocked("bluetooth") == Some(true) {
        airplane_btn.set_active(true);
    }
    {
        airplane_btn.connect_toggled(|btn| {
            let args: Vec<String> = if btn.is_active() {
                vec!["block".into(), "all".into()]
            } else {
                vec!["unblock".into(), "all".into()]
            };
            daemon_send(json!({"cmd":"run","program":"rfkill","args":args}));
        });
    }
    row1.append(&airplane_btn);

    vbox.append(&row1);

    // Row 2: Brightness slider + label
    let row2 = gtk::Box::new(Orientation::Horizontal, 8);
    row2.set_valign(Align::Center);
    let bright_label = gtk::Label::new(Some("Brightness:"));
    let brightness = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    brightness.set_value(50.0);
    brightness.set_hexpand(true);
    let max_brightness = read_max_brightness();
    {
        let max_brightness = max_brightness;
        brightness.connect_value_changed(move |s| {
            let pct = s.value() / 100.0;
            let val = (pct * max_brightness as f64).round() as u32;
            write_brightness(val);
        });
    }
    row2.append(&bright_label);
    row2.append(&brightness);
    vbox.append(&row2);

    // Row 3: Volume slider + label + mute
    let row3 = gtk::Box::new(Orientation::Horizontal, 8);
    row3.set_valign(Align::Center);
    let volume_label = gtk::Label::new(Some("Volume:"));
    let volume = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume.set_hexpand(true);
    let mute = gtk::ToggleButton::with_label("Mute");
    let sink = default_sink().to_string();
    {
        let sink = sink.clone();
        volume.connect_value_changed(move |s| {
            let val = s.value() as i32;
            if let Err(e) = Command::new("pactl")
                .args(&["set-sink-volume", &sink, &format!("{}%", val)])
                .spawn()
            {
                eprintln!("Failed to set volume: {}", e);
            }
        });
    }
    {
        let sink = sink.clone();
        mute.connect_toggled(move |_| {
            if let Err(e) = Command::new("pactl")
                .args(&["set-sink-mute", &sink, "toggle"])
                .spawn()
            {
                eprintln!("Failed to toggle mute: {}", e);
            }
        });
    }
    row3.append(&volume_label);
    row3.append(&volume);
    row3.append(&mute);
    vbox.append(&row3);

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    // Row 4 & 5: Resolution and refresh rate
    if let Some((output_name, modes)) = query_display_modes() {
        use std::collections::BTreeSet;
        let output_name = Rc::new(output_name);
        let modes = Rc::new(modes);
        let mut set = BTreeSet::new();
        for m in modes.iter() {
            set.insert((m.width, m.height));
        }
        let resolutions: Vec<(u32, u32)> = set.into_iter().collect();
        let res_strings: Vec<String> = resolutions
            .iter()
            .map(|(w, h)| format!("{}x{}", w, h))
            .collect();
        let res_refs: Vec<&str> = res_strings.iter().map(|s| s.as_str()).collect();
        let res_combo = gtk::DropDown::from_strings(&res_refs);

        let refresh_model = gtk::StringList::new(&[]);
        let refresh_combo = gtk::DropDown::new(Some(&refresh_model), None::<gtk::Expression>);
        let refresh_model = Rc::new(refresh_model);
        let resolutions = Rc::new(resolutions);

        let update_refresh: Rc<dyn Fn(u32)> = {
            let modes = modes.clone();
            let resolutions = resolutions.clone();
            let refresh_model = refresh_model.clone();
            let refresh_combo = refresh_combo.clone();
            let output_name = output_name.clone();
            Rc::new(move |idx: u32| {
                if let Some((w, h)) = resolutions.get(idx as usize) {
                    let mut rates: Vec<u32> = modes
                        .iter()
                        .filter(|m| m.width == *w && m.height == *h)
                        .map(|m| m.refresh)
                        .collect();
                    rates.sort();
                    rates.dedup();
                    while refresh_model.n_items() > 0 {
                        refresh_model.remove(refresh_model.n_items() - 1);
                    }
                    for r in &rates {
                        let disp = if r % 1000 == 0 {
                            format!("{} Hz", r / 1000)
                        } else {
                            format!("{:.1} Hz", *r as f64 / 1000.0)
                        };
                        refresh_model.append(&disp);
                    }
                    refresh_combo.set_selected(0);
                    if let Some(first) = rates.first() {
                        apply_display_mode(&output_name, *w, *h, *first);
                    }
                }
            })
        };

        update_refresh(0);

        res_combo.connect_selected_notify({
            let update_refresh = update_refresh.clone();
            move |combo| {
                update_refresh(combo.selected());
            }
        });

        refresh_combo.connect_selected_notify({
            let modes = modes.clone();
            let resolutions = resolutions.clone();
            let output_name = output_name.clone();
            let res_combo = res_combo.clone();
            move |combo| {
                let res_idx = res_combo.selected();
                if let Some((w, h)) = resolutions.get(res_idx as usize) {
                    let mut rates: Vec<u32> = modes
                        .iter()
                        .filter(|m| m.width == *w && m.height == *h)
                        .map(|m| m.refresh)
                        .collect();
                    rates.sort();
                    rates.dedup();
                    if let Some(r) = rates.get(combo.selected() as usize) {
                        apply_display_mode(&output_name, *w, *h, *r);
                    }
                }
            }
        });

        let row4 = gtk::Box::new(Orientation::Horizontal, 8);
        row4.append(&gtk::Label::new(Some("Resolution:")));
        row4.append(&res_combo);
        vbox.append(&row4);

        let row5 = gtk::Box::new(Orientation::Horizontal, 8);
        row5.append(&gtk::Label::new(Some("Refresh:")));
        row5.append(&refresh_combo);
        vbox.append(&row5);
    } else {
        let row4 = gtk::Box::new(Orientation::Horizontal, 8);
        let res_combo = gtk::DropDown::from_strings(&["1080p", "720p"]);
        row4.append(&gtk::Label::new(Some("Resolution:")));
        row4.append(&res_combo);
        vbox.append(&row4);

        let row5 = gtk::Box::new(Orientation::Horizontal, 8);
        for hz in ["40Hz", "50Hz", "60Hz"] {
            row5.append(&gtk::Button::with_label(hz));
        }
        vbox.append(&row5);
    }

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    // Row 6: TDP slider + snap & label
    let row6 = gtk::Box::new(Orientation::Horizontal, 8);
    row6.set_valign(Align::Center);
    let tdp_label = gtk::Label::new(Some("TDP (W):"));
    let tdp = gtk::Scale::with_range(Orientation::Horizontal, 5.0, 28.0, 1.0);
    tdp.set_value(15.0);
    tdp.set_hexpand(true);
    let tdp_value = gtk::Label::new(Some(&format!("{} W", tdp.value() as i32)));
    {
        let tdp_value_cl = tdp_value.clone();
        tdp.connect_value_changed(move |s| {
            let w = s.value().round() as i32;
            s.set_value(w as f64);
            tdp_value_cl.set_text(&format!("{} W", w));

            let stapm = format!("{}000", w);
            daemon_send(json!({"cmd":"run","program":"ryzenadj","args":["--stapm-limit", stapm]}));
        });
    }
    row6.append(&tdp_label);
    row6.append(&tdp);
    row6.append(&tdp_value);
    vbox.append(&row6);

    // Row 7: Fan profile radio‐style
    let row7 = gtk::Box::new(Orientation::Horizontal, 8);
    let auto = gtk::CheckButton::with_label("Auto");
    let quiet = gtk::CheckButton::with_label("Quiet");
    quiet.set_group(Some(&auto));
    let aggressive = gtk::CheckButton::with_label("Aggressive");
    aggressive.set_group(Some(&auto));
    let manual = gtk::CheckButton::with_label("Manual");
    manual.set_group(Some(&auto));
    row7.append(&auto);
    row7.append(&quiet);
    row7.append(&aggressive);
    row7.append(&manual);
    vbox.append(&row7);

    // Row 8: Manual fan speed
    let manual_speed = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    manual_speed.set_visible(false);
    {
        let ms = manual_speed.clone();
        manual.connect_toggled(move |btn| ms.set_visible(btn.is_active()));
    }

    let fan_base = pwm_base().map(|s| s.to_string());
    if let Some(base) = fan_base.clone() {
        eprintln!("Fan control base: {}", base);
        let profile_state: Arc<Mutex<Option<&'static [FanPoint; 5]>>> = Arc::new(Mutex::new(None));
        {
            let state = profile_state.clone();
            let base = base.clone();
            std::thread::spawn(move || loop {
                let prof = { *state.lock().unwrap() };
                if let Some(points) = prof {
                    if let Some(temp) = read_temp(&base) {
                        let pct = eval_curve(points, temp);
                        let pwm = ((pct / 100.0) * 255.0).round() as u8;
                        write_to_sysfs(&format!("{}/pwm1_enable", base), "1");
                        write_to_sysfs(&format!("{}/pwm1", base), pwm.to_string());
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            });
        }
        // Auto
        {
            let base = base.clone();
            let state = profile_state.clone();
            auto.connect_toggled(move |btn| {
                if btn.is_active() {
                    eprintln!("Auto mode active");
                    *state.lock().unwrap() = None;
                    write_to_sysfs(&format!("{}/pwm1_enable", base), "0");
                }
            });
        }
        // Quiet
        {
            let state = profile_state.clone();
            let base = base.clone();
            quiet.connect_toggled(move |btn| {
                if btn.is_active() {
                    eprintln!("Quiet mode active");
                    *state.lock().unwrap() = Some(&QUIET_CURVE);
                    write_to_sysfs(&format!("{}/pwm1_enable", base), "1");
                }
            });
        }
        // Aggressive
        {
            let state = profile_state.clone();
            let base = base.clone();
            aggressive.connect_toggled(move |btn| {
                if btn.is_active() {
                    eprintln!("Aggressive mode active");
                    *state.lock().unwrap() = Some(&AGGRESSIVE_CURVE);
                    write_to_sysfs(&format!("{}/pwm1_enable", base), "1");
                }
            });
        }
        // Manual
        {
            let base = base.clone();
            let ms = manual_speed.clone();
            let state = profile_state.clone();
            manual.connect_toggled(move |btn| {
                if btn.is_active() {
                    eprintln!("Manual mode active");
                    *state.lock().unwrap() = None;
                    write_to_sysfs(&format!("{}/pwm1_enable", base), "1");
                    let pct = ms.value() / 100.0;
                    let pwm = (pct * 255.0).round() as u8;
                    write_to_sysfs(&format!("{}/pwm1", base), pwm.to_string());
                }
            });
        }
        {
            let base = base.clone();
            let manual_btn = manual.clone();
            manual_speed.connect_value_changed(move |s| {
                if !manual_btn.is_active() {
                    return;
                }
                let pct = s.value();
                let pwm = ((pct / 100.0) * 255.0).round() as u8;
                eprintln!("Manual speed {}% -> {}", pct, pwm);
                write_to_sysfs(&format!("{}/pwm1_enable", base), "1");
                write_to_sysfs(&format!("{}/pwm1", base), pwm.to_string());
            });
        }
    } else {
        eprintln!("aynec hwmon device not found; disabling fan controls");
        auto.set_sensitive(false);
        quiet.set_sensitive(false);
        aggressive.set_sensitive(false);
        manual.set_sensitive(false);
        manual_speed.set_sensitive(false);
    }
    vbox.append(&manual_speed);

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    // RGB Lighting section
    let rgb_section = gtk::Box::new(Orientation::Vertical, 8);
    let rgb_label = gtk::Label::new(Some("RGB Lighting"));
    rgb_label.add_css_class("heading");
    rgb_section.append(&rgb_label);
    rgb_section.append(&gtk::Separator::new(Orientation::Horizontal));

    // Mode selection
    let mode_row = gtk::Box::new(Orientation::Horizontal, 8);
    mode_row.append(&gtk::Label::new(Some("Mode:")));
    let off_btn = gtk::CheckButton::with_label("Off");
    off_btn.set_active(true);
    let breathe_btn = gtk::CheckButton::with_label("Breathe");
    breathe_btn.set_group(Some(&off_btn));
    let manual_btn = gtk::CheckButton::with_label("Manual");
    manual_btn.set_group(Some(&off_btn));
    mode_row.append(&off_btn);
    mode_row.append(&breathe_btn);
    mode_row.append(&manual_btn);
    rgb_section.append(&mode_row);

    // Manual controls
    let manual_box = gtk::Box::new(Orientation::Vertical, 8);
    let hue = Rc::new(Cell::new(0.0f64));

    // Hue slider
    let hue_area = gtk::DrawingArea::new();
    hue_area.set_content_height(20);
    hue_area.set_hexpand(true);
    manual_box.append(&hue_area);

    // Brightness slider
    let bright_row = gtk::Box::new(Orientation::Horizontal, 4);
    bright_row.append(&gtk::Label::new(Some("Brightness:")));
    let brightness = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 255.0, 1.0);
    brightness.set_hexpand(true);
    brightness.set_value(255.0);
    bright_row.append(&brightness);
    manual_box.append(&bright_row);

    // Preview circle
    let preview = gtk::DrawingArea::new();
    preview.set_content_width(40);
    preview.set_content_height(40);
    manual_box.append(&preview);

    manual_box.set_visible(false);
    rgb_section.append(&manual_box);

    // Apply RGB changes
    let apply_settings = Rc::new({
        let hue = hue.clone();
        let brightness = brightness.clone();
        let preview = preview.clone();
        move || {
            let h = hue.get();
            let b = brightness.value() as u8;
            let (r, g, bl) = hsv_to_rgb(h, 1.0, 1.0);
            rgb_set_brightness(b);
            rgb_set_intensity(r, g, bl);
            preview.queue_draw();
        }
    });

    let pending = Rc::new(RefCell::new(None::<SourceId>));
    let schedule_apply = Rc::new({
        let apply_settings = apply_settings.clone();
        let pending_ref = pending.clone();
        move || {
            if pending_ref.borrow().is_none() {
                let apply = apply_settings.clone();
                let pending_clone = pending_ref.clone();
                let id = glib::timeout_add_local(Duration::from_millis(16), move || {
                    apply();
                    pending_clone.borrow_mut().take();
                    glib::ControlFlow::Break
                });
                *pending_ref.borrow_mut() = Some(id);
            }
        }
    });

    brightness.connect_value_changed({
        let schedule = schedule_apply.clone();
        move |_| schedule()
    });

    // Draw hue gradient and handle interaction
    {
        let hue = hue.clone();
        let schedule = schedule_apply.clone();
        let hue_area = hue_area.clone();
        let draw_hue = hue.clone();
        hue_area.set_draw_func(move |_w, cr, width, height| {
            let grad = cairo::LinearGradient::new(0.0, 0.0, width as f64, 0.0);
            let stops = [
                (0.0, 1.0, 0.0, 0.0),
                (1.0 / 6.0, 1.0, 1.0, 0.0),
                (2.0 / 6.0, 0.0, 1.0, 0.0),
                (3.0 / 6.0, 0.0, 1.0, 1.0),
                (4.0 / 6.0, 0.0, 0.0, 1.0),
                (5.0 / 6.0, 1.0, 0.0, 1.0),
                (1.0, 1.0, 0.0, 0.0),
            ];
            for (pos, r, g, b) in stops {
                grad.add_color_stop_rgb(pos, r, g, b);
            }
            let _ = cr.set_source(&grad);
            cr.rectangle(0.0, 0.0, width as f64, height as f64);
            let _ = cr.fill();

            let x = draw_hue.get() / 360.0 * width as f64;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.rectangle(x - 2.0, 0.0, 4.0, height as f64);
            let _ = cr.fill();
        });

        let update_hue = {
            let hue_area = hue_area.clone();
            let hue = hue.clone();
            let schedule = schedule.clone();
            move |x: f64| {
                let width = hue_area.allocated_width() as f64;
                let clamped = x.clamp(0.0, width);
                hue.set(clamped / width * 360.0);
                hue_area.queue_draw();
                schedule();
            }
        };

        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        {
            let update = update_hue.clone();
            drag.connect_drag_begin(move |_g, x, _y| update(x));
        }
        {
            let update = update_hue.clone();
            drag.connect_drag_update(move |g, dx, _dy| {
                if let Some((start_x, _)) = g.start_point() {
                    update(start_x + dx);
                }
            });
        }
        hue_area.add_controller(drag);
    }

    // Draw preview circle
    {
        let hue = hue.clone();
        let brightness = brightness.clone();
        preview.set_draw_func(move |_w, cr, width, height| {
            let (r, g, b) = hsv_to_rgb(hue.get(), 1.0, 1.0);
            let scale = brightness.value() / 255.0;
            cr.set_source_rgb(
                (r as f64 / 255.0) * scale,
                (g as f64 / 255.0) * scale,
                (b as f64 / 255.0) * scale,
            );
            let radius = (width.min(height) as f64) / 2.0;
            cr.arc(
                width as f64 / 2.0,
                height as f64 / 2.0,
                radius,
                0.0,
                std::f64::consts::PI * 2.0,
            );
            let _ = cr.fill();
        });
    }

    // Mode handler
    {
        let manual_box = manual_box.clone();
        off_btn.connect_toggled(move |btn| {
            if btn.is_active() {
                manual_box.set_visible(false);
                rgb_set_mode(1);
                rgb_set_brightness(0);
                rgb_set_intensity(0, 0, 0);
            }
        });
    }
    {
        let manual_box = manual_box.clone();
        breathe_btn.connect_toggled(move |btn| {
            if btn.is_active() {
                manual_box.set_visible(false);
                rgb_set_mode(0);
            }
        });
    }
    {
        let manual_box = manual_box.clone();
        let apply = apply_settings.clone();
        manual_btn.connect_toggled(move |btn| {
            if btn.is_active() {
                manual_box.set_visible(true);
                rgb_set_mode(1);
                apply();
            }
        });
    }

    vbox.append(&rgb_section);
    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    scrolled.set_child(Some(&vbox));
    window.set_child(Some(&scrolled));
    window.present();
}

pub fn run() {
    let app = Application::builder()
        .application_id("com.example.loki-control")
        .build();
    app.connect_activate(build_ui);
    app.run();
}

fn tokio_rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("tokio runtime"))
}

fn daemon_send(val: serde_json::Value) {
    let rt = tokio_rt();
    rt.spawn(async move {
        let msg = val.to_string() + "\n";
        for _ in 0..3 {
            match UnixStream::connect("/run/loki-master.sock").await {
                Ok(stream) => {
                    let mut stream = stream;
                    if stream.write_all(msg.as_bytes()).await.is_ok() {
                        let mut reader = BufReader::new(stream);
                        let mut resp = String::new();
                        let _ = reader.read_line(&mut resp).await;
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("connect failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    });
}
