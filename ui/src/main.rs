#[cfg(feature = "gui")]
mod gui;

#[cfg(feature = "gui")]
fn main() {
    gui::run();
}

#[cfg(not(feature = "gui"))]
fn main() {
    eprintln!("GUI feature disabled. Rebuild with --features gui");
}
