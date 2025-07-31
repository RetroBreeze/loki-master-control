use gdk4;
use gtk::glib;
use gtk::glib::clone;
use gtk::{prelude::*, Application, ApplicationWindow, Orientation};
use gtk4 as gtk;
use gtk4_layer_shell::{self as layer_shell, LayerShell};

fn build_ui(app: &Application) {
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

    if let Some(display) = gdk4::Display::default() {
        let monitors = display.monitors();
        if let Some(obj) = monitors.item(0) {
            if let Ok(monitor) = obj.downcast::<gdk4::Monitor>() {
                let geo = monitor.geometry();
                let width = ((geo.width() as f32) * 0.3) as i32;
                window.set_default_size(width, geo.height());
            }
        }
    }

    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }

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

    let row1 = gtk::Box::new(Orientation::Horizontal, 8);
    for label in ["Wi-Fi", "Bluetooth", "Airplane"] {
        let btn = gtk::Button::with_label(label);
        btn.add_css_class("circular");
        row1.append(&btn);
    }
    vbox.append(&row1);

    let brightness = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    brightness.set_value(50.0);
    vbox.append(&brightness);

    let row3 = gtk::Box::new(Orientation::Horizontal, 8);
    let volume = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume.set_hexpand(true);
    let mute = gtk::ToggleButton::with_label("Mute");
    row3.append(&volume);
    row3.append(&mute);
    vbox.append(&row3);

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

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

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    let tdp = gtk::Scale::with_range(Orientation::Horizontal, 5.0, 28.0, 1.0);
    tdp.set_value(15.0);
    vbox.append(&tdp);

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

    let manual_speed = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    manual_speed.set_value(50.0);
    manual_speed.set_visible(false);
    manual.connect_toggled(clone!(@weak manual_speed => move |btn| {
        manual_speed.set_visible(btn.is_active());
    }));
    vbox.append(&manual_speed);

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    // RGB Lighting section
    let rgb_section = gtk::Box::new(Orientation::Vertical, 8);

    let rgb_label = gtk::Label::new(Some("RGB Lighting"));
    rgb_label.add_css_class("heading");
    rgb_section.append(&rgb_label);
    rgb_section.append(&gtk::Separator::new(Orientation::Horizontal));

    let color_row = gtk::Box::new(Orientation::Horizontal, 8);
    let color_dialog = gtk::ColorDialog::new();
    let color_button = gtk::ColorDialogButton::new(Some(&color_dialog));
    color_button.add_css_class("circular");
    let preview = gtk::Box::new(Orientation::Vertical, 0);
    preview.set_size_request(24, 24);
    color_row.append(&color_button);
    color_row.append(&preview);
    rgb_section.append(&color_row);

    let advanced_switch = gtk::Switch::new();
    let advanced_row = gtk::Box::new(Orientation::Horizontal, 8);
    advanced_row.append(&gtk::Label::new(Some("Advanced sliders")));
    advanced_row.append(&advanced_switch);
    rgb_section.append(&advanced_row);

    let sliders = gtk::Box::new(Orientation::Vertical, 4);
    let red = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 255.0, 1.0);
    let green = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 255.0, 1.0);
    let blue = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 255.0, 1.0);
    for (label, scale) in [("Red:", &red), ("Green:", &green), ("Blue:", &blue)] {
        let row = gtk::Box::new(Orientation::Horizontal, 4);
        row.append(&gtk::Label::new(Some(label)));
        scale.set_hexpand(true);
        row.append(scale);
        sliders.append(&row);
    }
    sliders.set_visible(false);
    rgb_section.append(&sliders);

    advanced_switch.connect_state_set(
        clone!(@weak sliders => @default-return glib::Propagation::Proceed, move |_, state| {
            sliders.set_visible(state);
            glib::Propagation::Proceed
        }),
    );

    let brightness = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    rgb_section.append(&brightness);

    let effect_label = gtk::Label::new(Some("Effect: Static"));
    effect_label.set_sensitive(false);
    rgb_section.append(&effect_label);

    // update preview color when values change
    let css_provider = gtk::CssProvider::new();
    preview
        .style_context()
        .add_provider(&css_provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    let update_preview = move |color: gdk4::RGBA| {
        let css = format!("background-color:{};", color.to_string());
        css_provider.load_from_data(&css);
    };
    update_preview(color_button.rgba());
    color_button.connect_rgba_notify(clone!(@strong update_preview => move |btn| {
        update_preview(btn.rgba());
    }));
    for scale in [&red, &green, &blue] {
        scale.connect_value_changed(
            clone!(@weak color_button, @strong update_preview => move |_| {
                let mut color = color_button.rgba();
                color.red = red.value() as f32 / 255.0;
                color.green = green.value() as f32 / 255.0;
                color.blue = blue.value() as f32 / 255.0;
                color_button.set_rgba(&color);
            }),
        );
    }

    rgb_section.set_spacing(8);
    vbox.append(&rgb_section);

    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    let row10 = gtk::Box::new(Orientation::Horizontal, 8);
    let vibration = gtk::CheckButton::with_label("Vibration");
    row10.append(&vibration);
    row10.append(&gtk::Label::new(Some("Stick calibration coming soon")));
    vbox.append(&row10);

    scrolled.set_child(Some(&vbox));
    window.set_child(Some(&scrolled));

    window.show();
}

fn main() {
    let app = Application::builder()
        .application_id("com.example.loki-control")
        .build();
    app.connect_activate(build_ui);
    app.run();
}
