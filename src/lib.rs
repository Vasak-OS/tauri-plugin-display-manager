//! Brillo por monitor y configuración de la luz nocturna para aplicaciones de
//! Tauri.
//!
//! - Paneles internos: leídos de `/sys/class/backlight`, escritos por logind.
//! - Monitores externos: DDC/CI con `ddcutil --bus N`, el bus sacado de
//!   `/sys/class/drm` y guardado hasta que cambien los monitores.
//! - Cambios: el evento [`BRIGHTNESS_EVENT`], disparado por los uevents del
//!   kernel. Nada sondea.
//! - Luz nocturna: sólo la configuración de `wlsunset` ([`night_light`]).

use std::sync::Arc;

use tauri::{
    plugin::{Builder as PluginBuilder, TauriPlugin},
    AppHandle, Emitter, Manager, Runtime,
};

mod backlight;
mod cache;
mod commands;
mod ddc;
mod drm;
mod error;
mod manager;
mod models;
pub mod night_light;
#[cfg(test)]
mod testutil;
mod uevent;

pub use error::{Error, Result};
pub use manager::DisplayManager;
pub use models::*;

/// Acceso al gestor desde Rust.
pub trait DisplayManagerExt<R: Runtime> {
    fn display_manager(&self) -> &DisplayManager;
}

impl<R: Runtime, T: Manager<R>> DisplayManagerExt<R> for T {
    fn display_manager(&self) -> &DisplayManager {
        self.state::<DisplayManager>().inner()
    }
}

fn emitter<R: Runtime>(app: AppHandle<R>) -> manager::Notify {
    Arc::new(move |report: &BrightnessReport| {
        if let Err(e) = app.emit(BRIGHTNESS_EVENT, report) {
            log::warn!("display-manager: no se pudo emitir {BRIGHTNESS_EVENT}: {e}");
        }
    })
}

/// Opciones del plugin.
#[derive(Debug, Default)]
pub struct Builder {
    prefetch_ddc: bool,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Buscar los monitores externos al arrancar, en segundo plano, en vez de
    /// la primera vez que alguien pregunta. Para una aplicación que vive toda
    /// la sesión y muestra el brillo en cuanto se abre algo (el escritorio);
    /// una que se abre a demanda (Configuración) no lo necesita.
    pub fn prefetch_ddc(mut self, prefetch: bool) -> Self {
        self.prefetch_ddc = prefetch;
        self
    }

    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        let prefetch = self.prefetch_ddc;
        PluginBuilder::<R>::new("display-manager")
            .invoke_handler(tauri::generate_handler![
                commands::get_brightness,
                commands::set_brightness,
                commands::refresh_brightness,
                commands::get_night_light,
                commands::set_night_light,
            ])
            .setup(move |app, _api| {
                let manager = DisplayManager::new(emitter(app.clone()));

                let listener = manager.clone();
                if let Err(e) = uevent::spawn(move |event| listener.on_uevent(event)) {
                    log::warn!("display-manager: sin avisos del kernel: {e}");
                }
                if prefetch {
                    manager.refresh();
                }

                app.manage(manager);
                Ok(())
            })
            .build()
    }
}

/// Inicializa el plugin con las opciones por omisión.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new().build()
}
