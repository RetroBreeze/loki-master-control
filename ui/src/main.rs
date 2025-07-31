use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Application, ApplicationWindow, Orientation};
use gtk4 as gtk;
use gtk4_layer_shell::{self as layer_shell, LayerShell};
use libc;
use std::fs;
use std::process::Command;
use std::rc::Rc;
use std::sync::OnceLock;

static BRIGHTNESS_PATH: OnceLock<String> = OnceLock::new();
static MAX_BRIGHTNESS: OnceLock<u32> = OnceLock::new();
static DEFAULT_SINK: OnceLock<String> = OnceLock::new();

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
        if let Err(e) = fs::write(path, value.to_string()) {
            eprintln!("Failed to write {}: {}", path, e);
        }
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
            if let Err(e) = Command::new("rfkill").args(&["toggle", "wifi"]).spawn() {
                eprintln!("Failed to toggle Wi-Fi: {}", e);
            }
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
            if let Err(e) = Command::new("rfkill")
                .args(&["toggle", "bluetooth"])
                .spawn()
            {
                eprintln!("Failed to toggle Bluetooth: {}", e);
            }
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
            let args = if btn.is_active() {
                vec!["block", "all"]
            } else {
                vec!["unblock", "all"]
            };
            if let Err(e) = Command::new("rfkill").args(&args).spawn() {
                eprintln!("Failed to toggle airplane mode: {}", e);
            }
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

    // Row 4: Resolution dropdown
    let row4 = gtk::Box::new(Orientation::Horizontal, 8);
    let res_combo = gtk::DropDown::from_strings(&["1080p", "720p"]);
    row4.append(&gtk::Label::new(Some("Resolution:")));
    row4.append(&res_combo);
    vbox.append(&row4);

    // Row 5: Refresh rate buttons
    let row5 = gtk::Box::new(Orientation::Horizontal, 8);
    for hz in ["40Hz", "50Hz", "60Hz"] {
        row5.append(&gtk::Button::with_label(hz));
    }
    vbox.append(&row5);

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
            let mut cmd = if unsafe { libc::geteuid() } == 0 {
                Command::new("ryzenadj")
            } else {
                let mut c = Command::new("sudo");
                c.arg("ryzenadj");
                c
            };
            cmd.args(&["--stapm-limit", &stapm]);
            if let Err(e) = cmd.spawn() {
                eprintln!("Failed to run ryzenadj: {}", e);
            }
        });
    }
    row6.append(&tdp_label);
    row6.append(&tdp);
    row6.append(&tdp_value);
    vbox.append(&row6);

    // Row 7: Fan profile radio‐style
    let row7 = gtk::Box::new(Orientation::Horizontal, 8);
    let silent = gtk::CheckButton::with_label("Silent");
    let auto = gtk::CheckButton::with_label("Auto");
    let manual = gtk::CheckButton::with_label("Manual");
    auto.set_group(Some(&silent));
    manual.set_group(Some(&silent));
    row7.append(&silent);
    row7.append(&auto);
    row7.append(&manual);
    vbox.append(&row7);

    // Row 8: Manual fan speed
    let manual_speed = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    manual_speed.set_visible(false);
    {
        let ms = manual_speed.clone();
        manual.connect_toggled(move |btn| ms.set_visible(btn.is_active()));
    }
    vbox.append(&manual_speed);

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    // RGB Lighting section
    let rgb_section = gtk::Box::new(Orientation::Vertical, 8);
    let rgb_label = gtk::Label::new(Some("RGB Lighting"));
    rgb_label.add_css_class("heading");
    rgb_section.append(&rgb_label);
    rgb_section.append(&gtk::Separator::new(Orientation::Horizontal));

    // Color picker + preview
    let color_row = gtk::Box::new(Orientation::Horizontal, 8);
    let color_dialog = gtk::ColorDialog::new();
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    color_button.add_css_class("circular");
    let preview = gtk::Box::new(Orientation::Vertical, 0);
    preview.set_size_request(24, 24);
    preview.add_css_class("rgb-preview");
    color_row.append(&color_button);
    color_row.append(&preview);
    rgb_section.append(&color_row);

    // Advanced sliders toggle
    let advanced_switch = gtk::Switch::new();
    let advanced_row = gtk::Box::new(Orientation::Horizontal, 8);
    advanced_row.append(&gtk::Label::new(Some("Advanced sliders")));
    advanced_row.append(&advanced_switch);
    rgb_section.append(&advanced_row);

    // R/G/B sliders
    let sliders = gtk::Box::new(Orientation::Vertical, 4);
    let red = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 255.0, 1.0);
    let green = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 255.0, 1.0);
    let blue = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 255.0, 1.0);
    for (lbl, scale) in [("Red:", &red), ("Green:", &green), ("Blue:", &blue)] {
        let row = gtk::Box::new(Orientation::Horizontal, 4);
        row.append(&gtk::Label::new(Some(lbl)));
        scale.set_hexpand(true);
        row.append(scale);
        sliders.append(&row);
    }
    sliders.set_visible(false);
    {
        let sl = sliders.clone();
        advanced_switch.connect_state_set(move |_, on| {
            sl.set_visible(on);
            gtk::glib::Propagation::Proceed
        });
    }
    rgb_section.append(&sliders);

    // CSS provider for preview
    let css_provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // Wrap our preview‐updater in an Rc so we can clone it into multiple handlers:
    let update_preview = {
        let prov = css_provider.clone();
        Rc::new(move |color: gtk::gdk::RGBA| {
            let css = format!(
                ".rgb-preview {{ background-color: {}; }}",
                color.to_string()
            );
            prov.load_from_data(&css);
        })
    };

    // Initialize preview to current picker color
    (update_preview)(color_button.rgba());

    // Hook up color dialog changes
    {
        let upd = update_preview.clone();
        color_button.connect_rgba_notify(move |btn| {
            upd(btn.rgba());
        });
    }

    // Three independent R/G/B handlers:
    {
        let r = red.clone();
        let g = green.clone();
        let b = blue.clone();
        let cb = color_button.clone();
        let upd = update_preview.clone();
        red.connect_value_changed(move |_| {
            let mut c = cb.rgba();
            c.set_red(r.value() as f32 / 255.0);
            c.set_green(g.value() as f32 / 255.0);
            c.set_blue(b.value() as f32 / 255.0);
            cb.set_rgba(&c);
            upd(c);
        });
    }
    {
        let r = red.clone();
        let g = green.clone();
        let b = blue.clone();
        let cb = color_button.clone();
        let upd = update_preview.clone();
        green.connect_value_changed(move |_| {
            let mut c = cb.rgba();
            c.set_red(r.value() as f32 / 255.0);
            c.set_green(g.value() as f32 / 255.0);
            c.set_blue(b.value() as f32 / 255.0);
            cb.set_rgba(&c);
            upd(c);
        });
    }
    {
        let r = red.clone();
        let g = green.clone();
        let b = blue.clone();
        let cb = color_button.clone();
        let upd = update_preview.clone();
        blue.connect_value_changed(move |_| {
            let mut c = cb.rgba();
            c.set_red(r.value() as f32 / 255.0);
            c.set_green(g.value() as f32 / 255.0);
            c.set_blue(b.value() as f32 / 255.0);
            cb.set_rgba(&c);
            upd(c);
        });
    }

    vbox.append(&rgb_section);
    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    // Row 10: Vibration toggle
    let row10 = gtk::Box::new(Orientation::Horizontal, 8);
    let vibration = gtk::CheckButton::with_label("Vibration");
    row10.append(&vibration);
    row10.append(&gtk::Label::new(Some("Stick calibration coming soon")));
    vbox.append(&row10);

    scrolled.set_child(Some(&vbox));
    window.set_child(Some(&scrolled));
    window.present();
}

fn main() {
    let app = Application::builder()
        .application_id("com.example.loki-control")
        .build();
    app.connect_activate(build_ui);
    app.run();
}
