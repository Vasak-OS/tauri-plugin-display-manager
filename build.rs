const COMMANDS: &[&str] = &[
    "get_brightness",
    "set_brightness",
    "refresh_brightness",
    "get_night_light",
    "set_night_light",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
