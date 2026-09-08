// Prevent a second console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    copycat_gui_lib::run();
}
