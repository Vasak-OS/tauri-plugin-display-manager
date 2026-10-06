//! El gestor que guarda Tauri: junta la retroiluminación, la caché de DDC/CI y
//! los avisos del kernel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::OnceCell;
use zbus::Connection;

use crate::backlight::{self, BACKLIGHT_ROOT};
use crate::cache::{self, DdcCache, Job, Probe};
use crate::ddc;
use crate::drm::{self, Connector, DRM_ROOT};
use crate::models::{BrightnessKind, BrightnessReport, MonitorBrightness};
use crate::uevent::Uevent;
use crate::{Error, Result};

const LOGIND_DEST: &str = "org.freedesktop.login1";
/// Resuelve a la sesión de quien llama: no hace falta buscar el id.
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";

/// Quién se entera de que cambió el brillo. En el plugin es un `emit` de
/// Tauri; en las pruebas, cualquier cosa.
pub type Notify = Arc<dyn Fn(&BrightnessReport) + Send + Sync>;

/// De dónde se lee. Cambia sólo en las pruebas.
#[derive(Debug, Clone)]
pub struct Paths {
    pub drm: PathBuf,
    pub backlight: PathBuf,
    pub dev: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            drm: PathBuf::from(DRM_ROOT),
            backlight: PathBuf::from(BACKLIGHT_ROOT),
            dev: PathBuf::from("/dev"),
        }
    }
}

/// Para escribir sólo el último valor pedido a cada monitor.
///
/// Arrastrar un deslizador pide decenas de valores por segundo y cada
/// escritura por DDC/CI tarda 40 ms o más: en fila, el monitor seguiría
/// cambiando segundos después de soltar. Cada pedido saca un número; cuando le
/// toca, si ya hay uno más nuevo, no escribe nada.
#[derive(Debug, Default)]
pub struct Tickets(HashMap<u32, u64>);

impl Tickets {
    pub fn take(&mut self, bus: u32) -> u64 {
        let next = self.0.entry(bus).or_insert(0);
        *next += 1;
        *next
    }

    pub fn is_latest(&self, bus: u32, ticket: u64) -> bool {
        self.0.get(&bus) == Some(&ticket)
    }
}

struct Inner {
    paths: Paths,
    notify: Notify,
    cache: Mutex<DdcCache>,
    tickets: Mutex<Tickets>,
    /// Una sola conversación por DDC/CI a la vez: dos procesos de ddcutil
    /// sobre el mismo bus se pisan las respuestas.
    ddc_lock: tokio::sync::Mutex<()>,
    system_bus: OnceCell<Connection>,
}

#[derive(Clone)]
pub struct DisplayManager {
    inner: Arc<Inner>,
}

/// La máquina de verdad, para `cache::run`.
struct SystemProbe<'a> {
    paths: &'a Paths,
    ddcutil: Option<PathBuf>,
}

impl Probe for SystemProbe<'_> {
    fn connectors(&self) -> Vec<Connector> {
        drm::connected(&self.paths.drm)
    }
    fn has_ddcutil(&self) -> bool {
        self.ddcutil.is_some()
    }
    fn i2c_present(&self) -> bool {
        ddc::i2c_devices_present(&self.paths.dev)
    }
    fn can_open(&self, bus: u32) -> std::io::Result<()> {
        ddc::can_open_bus(&self.paths.dev, bus)
    }
    fn read(&self, bus: u32) -> Option<(u16, u16)> {
        let program = self.ddcutil.as_ref()?;
        ddc::run(program, &ddc::getvcp_args(bus))
            .map_err(|e| log::debug!("display-manager: bus {bus} no contesta: {e}"))
            .ok()
            .and_then(|out| ddc::parse_vcp(&out))
    }
    fn detect(&self) -> Option<String> {
        let program = self.ddcutil.as_ref()?;
        ddc::run(program, &ddc::detect_args()).ok()
    }
}

/// El brillo de los paneles internos: dos lecturas de sysfs por dispositivo.
pub fn backlight_monitors(paths: &Paths) -> Vec<MonitorBrightness> {
    let devices = backlight::list(&paths.backlight);
    // Los conectores sólo hacen falta si ningún dispositivo cuelga de uno.
    let connectors = if devices.iter().any(|d| d.connector.is_some()) {
        Vec::new()
    } else {
        drm::connected(&paths.drm)
    };
    backlight::choose(&devices, &connectors)
        .into_iter()
        .map(|(output, device)| MonitorBrightness {
            output,
            kind: BrightnessKind::Backlight,
            handle: device.name,
            percent: backlight::to_percent(device.brightness, device.max),
        })
        .collect()
}

impl DisplayManager {
    pub fn new(notify: Notify) -> Self {
        Self::with_paths(Paths::default(), notify)
    }

    pub fn with_paths(paths: Paths, notify: Notify) -> Self {
        Self {
            inner: Arc::new(Inner {
                paths,
                notify,
                cache: Mutex::new(DdcCache::default()),
                tickets: Mutex::new(Tickets::default()),
                ddc_lock: tokio::sync::Mutex::new(()),
                system_bus: OnceCell::new(),
            }),
        }
    }

    /// Lo que se sabe ahora, sin hablarle a ningún monitor.
    pub fn report(&self) -> BrightnessReport {
        let mut monitors = backlight_monitors(&self.inner.paths);
        let (external, ddc) = self
            .inner
            .cache
            .lock()
            .map(|cache| cache.report())
            .unwrap_or_else(|_| DdcCache::default().report());
        monitors.extend(external);
        BrightnessReport { monitors, ddc }
    }

    /// Lo que se sabe ahora y, si hace falta, una búsqueda en segundo plano
    /// cuyo resultado llega por el evento. Nunca espera a DDC/CI.
    pub fn get(&self) -> BrightnessReport {
        self.schedule(false);
        self.report()
    }

    /// Releer los monitores externos aunque lo guardado esté fresco.
    pub fn refresh(&self) {
        self.schedule(true);
    }

    fn notify(&self) {
        let report = self.report();
        (self.inner.notify)(&report);
    }

    fn schedule(&self, force: bool) {
        let job = self
            .inner
            .cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.begin(Instant::now(), force));
        if let Some(job) = job {
            let manager = self.clone();
            tauri::async_runtime::spawn(async move { manager.run_jobs(job).await });
        }
    }

    async fn run_jobs(&self, mut job: Job) {
        loop {
            let snapshot = {
                let _guard = self.inner.ddc_lock.lock().await;
                let inner = Arc::clone(&self.inner);
                let probe_job = job.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let probe = SystemProbe {
                        paths: &inner.paths,
                        ddcutil: ddc::ddcutil_path(),
                    };
                    cache::run(&probe_job, &probe)
                })
                .await
            };

            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(e) => {
                    // Un resultado vacío se leería como «no hay monitores
                    // externos» durante un minuto. Se descarta, y la próxima
                    // consulta vuelve a buscar.
                    log::warn!("display-manager: la búsqueda de monitores se cayó: {e}");
                    if let Ok(mut cache) = self.inner.cache.lock() {
                        cache.abandon(&job);
                    }
                    break;
                }
            };

            let next = {
                let Ok(mut cache) = self.inner.cache.lock() else {
                    return;
                };
                if cache.finish(&job, snapshot, Instant::now()) {
                    None
                } else {
                    // Cambiaron los monitores mientras se buscaba.
                    cache.begin(Instant::now(), false)
                }
            };

            match next {
                None => break,
                Some(again) => job = again,
            }
        }
        self.notify();
    }

    /// Lo que hay que hacer con un aviso del kernel.
    pub fn on_uevent(&self, event: Uevent) {
        match event {
            Uevent::Backlight => self.notify(),
            Uevent::DrmHotplug | Uevent::Lost => {
                let wanted = self
                    .inner
                    .cache
                    .lock()
                    .map(|mut cache| cache.invalidate())
                    .unwrap_or(false);
                // Avisa ya, con los externos «buscando», y busca si alguien
                // llegó a preguntar alguna vez.
                self.notify();
                if wanted {
                    self.schedule(false);
                }
            }
        }
    }

    async fn system_bus(&self) -> Result<&Connection> {
        Ok(self
            .inner
            .system_bus
            .get_or_try_init(Connection::system)
            .await?)
    }

    pub async fn set(&self, kind: BrightnessKind, handle: &str, percent: u8) -> Result<()> {
        match kind {
            BrightnessKind::Backlight => self.set_backlight(handle, percent).await,
            BrightnessKind::Ddc => self.set_ddc(handle, percent).await,
        }
    }

    /// Por logind, que deja al usuario de la sesión activa escribir su propia
    /// retroiluminación sin root ni diálogo. El aviso del cambio llega solo:
    /// la escritura dispara un uevent.
    async fn set_backlight(&self, device: &str, percent: u8) -> Result<()> {
        let max = backlight::list(&self.inner.paths.backlight)
            .into_iter()
            .find(|d| d.name == device)
            .map(|d| d.max)
            .ok_or_else(|| Error::UnknownBacklight(device.to_string()))?;
        let raw = backlight::to_raw(percent, max);

        let bus = self.system_bus().await?;
        zbus::Proxy::new(bus, LOGIND_DEST, SESSION_PATH, SESSION_IFACE)
            .await?
            .call::<_, _, ()>("SetBrightness", &("backlight", device, raw))
            .await?;
        Ok(())
    }

    async fn set_ddc(&self, handle: &str, percent: u8) -> Result<()> {
        let unknown = || Error::UnknownDisplay(handle.to_string());
        let bus: u32 = handle.parse().map_err(|_| unknown())?;
        let ticket = self
            .inner
            .tickets
            .lock()
            .map(|mut t| t.take(bus))
            .map_err(|_| unknown())?;

        let _guard = self.inner.ddc_lock.lock().await;
        let superseded = self
            .inner
            .tickets
            .lock()
            .map(|t| !t.is_latest(bus, ticket))
            .unwrap_or(false);
        if superseded {
            return Ok(());
        }

        let display = self
            .inner
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.display(bus))
            .ok_or_else(unknown)?;
        let program = ddc::ddcutil_path().ok_or_else(|| Error::Ddcutil("not installed".into()))?;
        let value = ddc::to_raw(percent, display.max);

        tauri::async_runtime::spawn_blocking(move || {
            ddc::run(&program, &ddc::setvcp_args(bus, value))
        })
        .await
        .map_err(|e| Error::Ddcutil(e.to_string()))??;

        if let Ok(mut cache) = self.inner.cache.lock() {
            cache.set_current(bus, value);
        }
        self.notify();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DdcState, DdcStatus};
    use crate::testutil::TempTree;

    fn paths(tree: &TempTree) -> Paths {
        Paths {
            drm: tree.dir("drm"),
            backlight: tree.dir("class/backlight"),
            dev: tree.dir("dev"),
        }
    }

    fn laptop(tree: &TempTree) {
        tree.file("drm/card1-eDP-1/status", "connected\n");
        tree.file("devices/intel_backlight/brightness", "4800\n");
        tree.file("devices/intel_backlight/max_brightness", "19200\n");
        tree.file("devices/intel_backlight/type", "raw\n");
        std::os::unix::fs::symlink(
            tree.path().join("devices/intel_backlight"),
            tree.path().join("class/backlight/intel_backlight"),
        )
        .unwrap();
    }

    #[test]
    fn solo_saca_numeros_nuevos_y_gana_el_ultimo() {
        let mut tickets = Tickets::default();
        let first = tickets.take(5);
        let second = tickets.take(5);
        let other = tickets.take(7);
        assert!(!tickets.is_latest(5, first), "lo pisó el segundo");
        assert!(tickets.is_latest(5, second));
        assert!(tickets.is_latest(7, other), "cada monitor va por su lado");
        assert!(!tickets.is_latest(9, 1));
    }

    #[test]
    fn el_panel_interno_toma_el_conector_interno() {
        let tree = TempTree::new();
        let paths = paths(&tree);
        laptop(&tree);

        assert_eq!(
            backlight_monitors(&paths),
            vec![MonitorBrightness {
                output: Some("eDP-1".into()),
                kind: BrightnessKind::Backlight,
                handle: "intel_backlight".into(),
                percent: 25,
            }]
        );
    }

    #[test]
    fn un_escritorio_sin_panel_no_tiene_retroiluminacion() {
        let tree = TempTree::new();
        let paths = paths(&tree);
        tree.file("drm/card0-DP-1/status", "connected\n");
        assert!(backlight_monitors(&paths).is_empty());
    }

    #[test]
    fn el_informe_no_espera_a_ddc() {
        let tree = TempTree::new();
        let manager = DisplayManager::with_paths(paths(&tree), Arc::new(|_| {}));
        laptop(&tree);

        let report = manager.report();
        assert_eq!(report.monitors.len(), 1);
        assert_eq!(
            report.ddc,
            DdcStatus {
                state: DdcState::Detecting,
                reason: None,
                unsupported: vec![],
            },
            "todavía nadie buscó"
        );
    }

    #[test]
    fn un_cambio_de_brillo_avisa_con_el_valor_nuevo() {
        let tree = TempTree::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let manager = DisplayManager::with_paths(
            paths(&tree),
            Arc::new(move |r: &BrightnessReport| sink.lock().unwrap().push(r.clone())),
        );
        laptop(&tree);

        tree.file("devices/intel_backlight/brightness", "19200\n");
        manager.on_uevent(Uevent::Backlight);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].monitors[0].percent, 100);
    }

    #[test]
    fn un_monitor_nuevo_sin_nadie_mirando_no_lanza_busqueda() {
        let tree = TempTree::new();
        let count = Arc::new(Mutex::new(0));
        let sink = Arc::clone(&count);
        let manager = DisplayManager::with_paths(
            paths(&tree),
            Arc::new(move |_: &BrightnessReport| *sink.lock().unwrap() += 1),
        );

        manager.on_uevent(Uevent::DrmHotplug);
        assert_eq!(*count.lock().unwrap(), 1, "avisa igual");
        assert!(
            manager
                .inner
                .cache
                .lock()
                .unwrap()
                .begin(Instant::now(), false)
                .is_some(),
            "y no dejó ninguna búsqueda en curso"
        );
    }

    /// Contra el hardware de esta máquina; a mano, con `cargo test --
    /// --ignored --nocapture`. Escribe en el panel el mismo brillo que tiene.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn el_kernel_avisa_lo_que_escribe_logind() {
        let manager = DisplayManager::new(Arc::new(|_| {}));

        let started = Instant::now();
        let report = manager.report();
        println!("primer informe en {:?}: {report:?}", started.elapsed());
        let started = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(manager.report());
        }
        println!("informe, promedio de 1000: {:?}", started.elapsed() / 1000);
        let started = Instant::now();
        let job = DdcCache::default().begin(Instant::now(), false).unwrap();
        let probe = SystemProbe {
            paths: &Paths::default(),
            ddcutil: ddc::ddcutil_path(),
        };
        let snapshot = cache::run(&job, &probe);
        println!(
            "búsqueda de externos en {:?}: {snapshot:?}",
            started.elapsed()
        );
        let panel = report
            .monitors
            .iter()
            .find(|m| m.kind == BrightnessKind::Backlight)
            .expect("¿esta máquina tiene panel interno?")
            .clone();

        let (tx, rx) = std::sync::mpsc::channel();
        crate::uevent::spawn(move |event| {
            let _ = tx.send(event);
        })
        .expect("socket de uevents");

        manager
            .set(BrightnessKind::Backlight, &panel.handle, panel.percent)
            .await
            .expect("SetBrightness de logind");

        let event = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("el kernel no avisó");
        assert_eq!(event, Uevent::Backlight);
    }

    #[tokio::test]
    async fn escribir_en_un_monitor_desconocido_falla_sin_lanzar_ddcutil() {
        let tree = TempTree::new();
        let manager = DisplayManager::with_paths(paths(&tree), Arc::new(|_| {}));
        assert!(matches!(
            manager.set(BrightnessKind::Ddc, "7", 50).await,
            Err(Error::UnknownDisplay(h)) if h == "7"
        ));
        assert!(matches!(
            manager.set(BrightnessKind::Ddc, "DP-1", 50).await,
            Err(Error::UnknownDisplay(_))
        ));
        assert!(matches!(
            manager.set(BrightnessKind::Backlight, "nope", 50).await,
            Err(Error::UnknownBacklight(_))
        ));
    }
}
