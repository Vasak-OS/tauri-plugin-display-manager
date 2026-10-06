//! Lo que se sabe de los monitores externos, y cuándo hay que volver a
//! averiguarlo.
//!
//! Hablarle a un monitor por DDC/CI tarda (40 ms como mínimo por transacción,
//! segundos si el monitor no contesta), así que nunca se hace en el camino de
//! una consulta: `get_brightness` devuelve lo guardado y, si hace falta, pide
//! una búsqueda en segundo plano cuyo resultado llega como evento.
//!
//! - **Topología** (qué conector va en qué bus y cuál contesta): se averigua
//!   una vez y vale hasta que el kernel avisa que cambiaron los monitores
//!   (`uevent.rs`). No hay sondeo.
//! - **Valores** (el brillo de cada uno): se releen si tienen más de
//!   [`STALE_AFTER`] —alguien pudo tocar los botones del monitor, y eso no
//!   avisa—, también en segundo plano, y se actualizan al escribirlos.

use std::io;
use std::time::{Duration, Instant};

use crate::drm::{self, Connector};
use crate::models::{DdcState, DdcStatus, DdcUnavailableReason, MonitorBrightness};
use crate::{ddc, models::BrightnessKind};

/// Cuánto vale un brillo leído por DDC antes de volver a leerlo cuando alguien
/// pregunta.
pub const STALE_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdcDisplay {
    pub connector: String,
    pub bus: u32,
    pub current: u16,
    pub max: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub displays: Vec<DdcDisplay>,
    pub unsupported: Vec<String>,
    pub reason: Option<DdcUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    generation: u64,
    /// `None`: buscar todo desde cero. `Some`: la topología se conoce, sólo
    /// releer los valores.
    pub known: Option<Snapshot>,
}

#[derive(Debug, Default)]
pub struct DdcCache {
    generation: u64,
    snapshot: Option<(Snapshot, Instant)>,
    busy: bool,
    wanted: bool,
}

impl DdcCache {
    /// Pide un trabajo si hace falta y nadie lo está haciendo ya. `force` es
    /// «volver a leer aunque esté fresco» (el botón de la interfaz).
    pub fn begin(&mut self, now: Instant, force: bool) -> Option<Job> {
        self.wanted = true;
        if self.busy {
            return None;
        }
        let known = match &self.snapshot {
            None => None,
            Some((snapshot, read_at)) => {
                if !force && now.saturating_duration_since(*read_at) < STALE_AFTER {
                    return None;
                }
                Some(snapshot.clone())
            }
        };
        self.busy = true;
        Some(Job {
            generation: self.generation,
            known,
        })
    }

    /// Guarda el resultado de un trabajo. Devuelve falso si los monitores
    /// cambiaron mientras tanto: ese resultado describe un escritorio que ya
    /// no existe, se descarta, y quien llama tiene que pedir otro trabajo.
    pub fn finish(&mut self, job: &Job, result: Snapshot, now: Instant) -> bool {
        self.busy = false;
        if job.generation != self.generation {
            return false;
        }
        self.snapshot = Some((result, now));
        true
    }

    /// Cambiaron los monitores. Devuelve si alguien llegó a preguntar alguna
    /// vez: si nadie lo hizo, no vale la pena buscar hasta que pregunten.
    pub fn invalidate(&mut self) -> bool {
        self.generation += 1;
        self.snapshot = None;
        self.wanted
    }

    /// Lo que se acaba de escribir en un monitor ya es su valor.
    pub fn set_current(&mut self, bus: u32, current: u16) {
        if let Some((snapshot, _)) = &mut self.snapshot {
            for display in snapshot.displays.iter_mut().filter(|d| d.bus == bus) {
                display.current = current.min(display.max);
            }
        }
    }

    pub fn display(&self, bus: u32) -> Option<DdcDisplay> {
        self.snapshot
            .as_ref()?
            .0
            .displays
            .iter()
            .find(|d| d.bus == bus)
            .cloned()
    }

    /// Los monitores externos y el estado de la búsqueda, como los ve el
    /// frontend.
    pub fn report(&self) -> (Vec<MonitorBrightness>, DdcStatus) {
        match &self.snapshot {
            None => (
                Vec::new(),
                DdcStatus {
                    state: DdcState::Detecting,
                    reason: None,
                    unsupported: Vec::new(),
                },
            ),
            Some((snapshot, _)) => (
                snapshot
                    .displays
                    .iter()
                    .map(|d| MonitorBrightness {
                        output: Some(d.connector.clone()),
                        kind: BrightnessKind::Ddc,
                        handle: d.bus.to_string(),
                        percent: ddc::to_percent(d.current, d.max),
                    })
                    .collect(),
                DdcStatus {
                    state: if snapshot.reason.is_some() {
                        DdcState::Unavailable
                    } else {
                        DdcState::Ready
                    },
                    reason: snapshot.reason,
                    unsupported: snapshot.unsupported.clone(),
                },
            ),
        }
    }
}

/// Lo que la búsqueda necesita del sistema. En el plugin es sysfs, `/dev` y
/// ddcutil; en las pruebas, una máquina de mentira.
pub trait Probe {
    fn connectors(&self) -> Vec<Connector>;
    fn has_ddcutil(&self) -> bool;
    fn i2c_present(&self) -> bool;
    fn can_open(&self, bus: u32) -> io::Result<()>;
    /// `ddcutil --bus N getvcp 10`: actual y máximo, o nada si no contesta.
    fn read(&self, bus: u32) -> Option<(u16, u16)>;
    /// `ddcutil detect --brief`, el respaldo caro.
    fn detect(&self) -> Option<String>;
}

/// Hace un trabajo: buscar todo, o releer los valores de lo conocido.
pub fn run(job: &Job, probe: &dyn Probe) -> Snapshot {
    match &job.known {
        Some(known) if known.reason.is_none() => reread(known, probe),
        // Si antes faltaba algo (ddcutil, permisos), se vuelve a buscar
        // entero: puede que ya no falte.
        _ => detect(probe),
    }
}

fn reread(known: &Snapshot, probe: &dyn Probe) -> Snapshot {
    let mut snapshot = known.clone();
    for display in &mut snapshot.displays {
        // Si ahora no contesta (ocupado, en reposo), queda el último valor:
        // la topología no cambió, porque eso lo avisa el kernel.
        if let Some((current, max)) = probe.read(display.bus) {
            display.current = current;
            display.max = max;
        }
    }
    snapshot
}

fn detect(probe: &dyn Probe) -> Snapshot {
    let external: Vec<Connector> = probe
        .connectors()
        .into_iter()
        .filter(|c| !drm::is_internal(&c.name))
        .collect();

    // Sin monitores externos no hay nada que decir: ni que falta ddcutil.
    if external.is_empty() {
        return Snapshot::default();
    }

    let unavailable = |reason| Snapshot {
        reason: Some(reason),
        ..Snapshot::default()
    };
    if !probe.has_ddcutil() {
        return unavailable(DdcUnavailableReason::NotInstalled);
    }
    if !probe.i2c_present() {
        return unavailable(DdcUnavailableReason::NoI2cDev);
    }

    // El bus de cada uno sale de sysfs. Sólo si alguno no lo publica se paga
    // `ddcutil detect`, una vez.
    let mut fallback: Option<Vec<ddc::Detected>> = None;
    let mut snapshot = Snapshot::default();
    let mut denied = false;

    for connector in external {
        let bus = connector.ddc_bus.or_else(|| {
            fallback
                .get_or_insert_with(|| {
                    probe
                        .detect()
                        .map(|out| ddc::parse_detect(&out))
                        .unwrap_or_default()
                })
                .iter()
                .find(|d| d.connector == connector.name)
                .map(|d| d.bus)
        });

        let Some(bus) = bus else {
            snapshot.unsupported.push(connector.name);
            continue;
        };

        if let Err(e) = probe.can_open(bus) {
            if e.kind() == io::ErrorKind::PermissionDenied {
                denied = true;
            }
            snapshot.unsupported.push(connector.name);
            continue;
        }

        match probe.read(bus) {
            Some((current, max)) => snapshot.displays.push(DdcDisplay {
                connector: connector.name,
                bus,
                current,
                max,
            }),
            None => snapshot.unsupported.push(connector.name),
        }
    }

    // Si no se pudo abrir ningún bus por permisos, el problema no es de cada
    // monitor sino del usuario: eso es lo que hay que decir.
    if denied && snapshot.displays.is_empty() {
        return unavailable(DdcUnavailableReason::NoPermission);
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeMachine {
        connectors: Vec<Connector>,
        no_ddcutil: bool,
        no_i2c: bool,
        denied: Vec<u32>,
        values: HashMap<u32, (u16, u16)>,
        detect_output: Option<String>,
        detect_calls: Cell<u32>,
        reads: RefCell<Vec<u32>>,
    }

    impl Probe for FakeMachine {
        fn connectors(&self) -> Vec<Connector> {
            self.connectors.clone()
        }
        fn has_ddcutil(&self) -> bool {
            !self.no_ddcutil
        }
        fn i2c_present(&self) -> bool {
            !self.no_i2c
        }
        fn can_open(&self, bus: u32) -> io::Result<()> {
            if self.denied.contains(&bus) {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(())
            }
        }
        fn read(&self, bus: u32) -> Option<(u16, u16)> {
            self.reads.borrow_mut().push(bus);
            self.values.get(&bus).copied()
        }
        fn detect(&self) -> Option<String> {
            self.detect_calls.set(self.detect_calls.get() + 1);
            self.detect_output.clone()
        }
    }

    fn connector(name: &str, bus: Option<u32>) -> Connector {
        Connector {
            name: name.into(),
            ddc_bus: bus,
        }
    }

    fn fresh() -> Job {
        Job {
            generation: 0,
            known: None,
        }
    }

    #[test]
    fn una_notebook_sola_no_prueba_nada() {
        let machine = FakeMachine {
            connectors: vec![connector("eDP-1", Some(6))],
            no_ddcutil: true,
            ..Default::default()
        };
        assert_eq!(run(&fresh(), &machine), Snapshot::default());
        assert!(
            machine.reads.borrow().is_empty(),
            "el panel interno no habla DDC: probarlo tarda segundos"
        );
    }

    #[test]
    fn el_bus_sale_de_sysfs_sin_ddcutil_detect() {
        let machine = FakeMachine {
            connectors: vec![
                connector("eDP-1", Some(6)),
                connector("DP-2", Some(4)),
                connector("HDMI-A-1", Some(5)),
            ],
            values: HashMap::from([(4, (45, 100)), (5, (128, 255))]),
            ..Default::default()
        };
        let snapshot = run(&fresh(), &machine);
        assert_eq!(machine.detect_calls.get(), 0);
        assert_eq!(
            snapshot.displays,
            vec![
                DdcDisplay {
                    connector: "DP-2".into(),
                    bus: 4,
                    current: 45,
                    max: 100
                },
                DdcDisplay {
                    connector: "HDMI-A-1".into(),
                    bus: 5,
                    current: 128,
                    max: 255
                },
            ]
        );
        assert!(snapshot.unsupported.is_empty());
        assert_eq!(snapshot.reason, None);
    }

    #[test]
    fn el_respaldo_de_detect_corre_una_sola_vez() {
        let machine = FakeMachine {
            connectors: vec![
                connector("DP-1", None),
                connector("DP-2", None),
                connector("DP-3", None),
            ],
            detect_output: Some(
                "Display 1\n   I2C bus: /dev/i2c-9\n   DRM connector: card0-DP-1\n\
                 Display 2\n   I2C bus: /dev/i2c-10\n   DRM connector: card0-DP-2\n"
                    .into(),
            ),
            values: HashMap::from([(9, (30, 100)), (10, (70, 100))]),
            ..Default::default()
        };
        let snapshot = run(&fresh(), &machine);
        assert_eq!(machine.detect_calls.get(), 1, "una vez para los tres");
        assert_eq!(
            snapshot
                .displays
                .iter()
                .map(|d| (d.connector.as_str(), d.bus))
                .collect::<Vec<_>>(),
            vec![("DP-1", 9), ("DP-2", 10)]
        );
        assert_eq!(snapshot.unsupported, vec!["DP-3"]);
    }

    #[test]
    fn un_monitor_que_no_contesta_queda_como_no_disponible() {
        let machine = FakeMachine {
            connectors: vec![connector("DP-1", Some(3)), connector("HDMI-A-1", Some(2))],
            values: HashMap::from([(3, (50, 100))]),
            ..Default::default()
        };
        let snapshot = run(&fresh(), &machine);
        assert_eq!(snapshot.displays.len(), 1);
        assert_eq!(snapshot.unsupported, vec!["HDMI-A-1"]);
        assert_eq!(snapshot.reason, None);
    }

    #[test]
    fn dice_por_que_no_se_puede() {
        let base = || FakeMachine {
            connectors: vec![connector("DP-1", Some(3))],
            values: HashMap::from([(3, (50, 100))]),
            ..Default::default()
        };

        let machine = FakeMachine {
            no_ddcutil: true,
            ..base()
        };
        assert_eq!(
            run(&fresh(), &machine).reason,
            Some(DdcUnavailableReason::NotInstalled)
        );

        let machine = FakeMachine {
            no_i2c: true,
            ..base()
        };
        assert_eq!(
            run(&fresh(), &machine).reason,
            Some(DdcUnavailableReason::NoI2cDev)
        );

        let machine = FakeMachine {
            denied: vec![3],
            ..base()
        };
        let snapshot = run(&fresh(), &machine);
        assert_eq!(snapshot.reason, Some(DdcUnavailableReason::NoPermission));
        assert!(
            machine.reads.borrow().is_empty(),
            "sin permiso no se lanza ddcutil"
        );
    }

    #[test]
    fn releer_no_vuelve_a_buscar_y_conserva_lo_que_no_contesta() {
        let known = Snapshot {
            displays: vec![
                DdcDisplay {
                    connector: "DP-1".into(),
                    bus: 3,
                    current: 10,
                    max: 100,
                },
                DdcDisplay {
                    connector: "DP-2".into(),
                    bus: 4,
                    current: 20,
                    max: 100,
                },
            ],
            unsupported: vec!["HDMI-A-1".into()],
            reason: None,
        };
        let machine = FakeMachine {
            values: HashMap::from([(3, (60, 100))]),
            ..Default::default()
        };
        let job = Job {
            generation: 0,
            known: Some(known.clone()),
        };
        let snapshot = run(&job, &machine);
        assert_eq!(machine.detect_calls.get(), 0);
        assert_eq!(*machine.reads.borrow(), vec![3, 4], "sólo los conocidos");
        assert_eq!(snapshot.displays[0].current, 60);
        assert_eq!(snapshot.displays[1].current, 20, "queda el último valor");
        assert_eq!(snapshot.unsupported, known.unsupported);
    }

    #[test]
    fn si_antes_faltaba_algo_se_busca_de_nuevo() {
        let machine = FakeMachine {
            connectors: vec![connector("DP-1", Some(3))],
            values: HashMap::from([(3, (50, 100))]),
            ..Default::default()
        };
        let job = Job {
            generation: 0,
            known: Some(Snapshot {
                reason: Some(DdcUnavailableReason::NotInstalled),
                ..Snapshot::default()
            }),
        };
        assert_eq!(run(&job, &machine).displays.len(), 1);
    }

    #[test]
    fn la_primera_consulta_pide_buscar_y_la_segunda_no_duplica() {
        let mut cache = DdcCache::default();
        let now = Instant::now();

        let (monitors, status) = cache.report();
        assert!(monitors.is_empty());
        assert_eq!(status.state, DdcState::Detecting);

        let job = cache.begin(now, false).expect("hay que buscar");
        assert_eq!(job.known, None);
        assert_eq!(
            cache.begin(now, false),
            None,
            "ya hay una búsqueda en curso"
        );
        assert_eq!(cache.begin(now, true), None, "ni forzando");

        assert!(cache.finish(&job, Snapshot::default(), now));
        assert_eq!(cache.report().1.state, DdcState::Ready);
        assert_eq!(cache.begin(now, false), None, "fresco: no se relee");
    }

    #[test]
    fn un_valor_viejo_se_relee_y_forzar_relee_siempre() {
        let mut cache = DdcCache::default();
        let start = Instant::now();
        let job = cache.begin(start, false).unwrap();
        let snapshot = Snapshot {
            displays: vec![DdcDisplay {
                connector: "DP-1".into(),
                bus: 3,
                current: 40,
                max: 100,
            }],
            ..Snapshot::default()
        };
        cache.finish(&job, snapshot.clone(), start);

        let later = start + STALE_AFTER;
        let job = cache.begin(later, false).expect("viejo: se relee");
        assert_eq!(job.known.as_ref(), Some(&snapshot));
        cache.finish(&job, snapshot.clone(), later);

        assert!(cache.begin(later, true).is_some());
    }

    #[test]
    fn un_resultado_de_antes_del_cambio_de_monitores_se_descarta() {
        let mut cache = DdcCache::default();
        let now = Instant::now();
        let job = cache.begin(now, false).unwrap();

        assert!(cache.invalidate(), "alguien había preguntado");

        let stale = Snapshot {
            unsupported: vec!["DP-1".into()],
            ..Snapshot::default()
        };
        assert!(!cache.finish(&job, stale, now));
        assert_eq!(cache.report().1.state, DdcState::Detecting);

        let again = cache.begin(now, false).expect("hay que buscar de nuevo");
        assert_eq!(again.known, None);
        assert!(cache.finish(&again, Snapshot::default(), now));
    }

    #[test]
    fn sin_nadie_que_pregunte_un_cambio_no_dispara_busqueda() {
        let mut cache = DdcCache::default();
        assert!(!cache.invalidate());
    }

    #[test]
    fn lo_escrito_se_ve_en_el_informe() {
        let mut cache = DdcCache::default();
        let now = Instant::now();
        let job = cache.begin(now, false).unwrap();
        cache.finish(
            &job,
            Snapshot {
                displays: vec![DdcDisplay {
                    connector: "HDMI-A-1".into(),
                    bus: 5,
                    current: 0,
                    max: 255,
                }],
                unsupported: vec!["DP-1".into()],
                reason: None,
            },
            now,
        );

        cache.set_current(5, 128);
        cache.set_current(9, 10);
        let (monitors, status) = cache.report();
        assert_eq!(
            monitors,
            vec![MonitorBrightness {
                output: Some("HDMI-A-1".into()),
                kind: BrightnessKind::Ddc,
                handle: "5".into(),
                percent: 50,
            }]
        );
        assert_eq!(status.unsupported, vec!["DP-1"]);
        assert_eq!(cache.display(5).unwrap().current, 128);
        assert_eq!(cache.display(9), None);
    }
}
