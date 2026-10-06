use tauri::State;

use crate::manager::DisplayManager;
use crate::models::{BrightnessKind, BrightnessReport, NightLight, NightLightConfig};
use crate::night_light;
use crate::Result;

/// El brillo de cada pantalla que se puede atenuar. Los paneles internos se
/// leen de sysfs en el momento; los monitores externos salen de lo guardado y,
/// si hace falta, se buscan en segundo plano (llegan por el evento). Nunca
/// espera a DDC/CI.
#[tauri::command]
pub fn get_brightness(state: State<'_, DisplayManager>) -> BrightnessReport {
    state.get()
}

#[tauri::command]
pub async fn set_brightness(
    state: State<'_, DisplayManager>,
    kind: BrightnessKind,
    handle: String,
    percent: u8,
) -> Result<()> {
    state.set(kind, &handle, percent).await
}

/// Vuelve a leer los monitores externos aunque lo guardado esté fresco. El
/// resultado llega por el evento.
#[tauri::command]
pub fn refresh_brightness(state: State<'_, DisplayManager>) {
    state.refresh();
}

#[tauri::command]
pub fn get_night_light() -> Result<NightLight> {
    Ok(night_light::read(&night_light::unit_path()?))
}

/// Valida y guarda la configuración. No enciende ni apaga el servicio.
#[tauri::command]
pub fn set_night_light(config: NightLightConfig) -> Result<NightLight> {
    night_light::write(&night_light::unit_path()?, &config)
}
