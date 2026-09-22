#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The main window is declared once in tauri.conf.json.
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("failed to run standalone tool");
}
