//! Los monitores externos, por DDC/CI a través de `ddcutil`.
//!
//! ## Por qué el binario y no `libddcutil`
//!
//! Medido en la máquina de desarrollo (ddcutil 3.0.2, libddcutil 5.6.2):
//!
//! | | costo |
//! |---|---|
//! | arrancar `ddcutil` y salir (`--version`) | 4–5 ms |
//! | `ddcutil --bus N getvcp 10` sobre un bus sin monitor (arranque + inicialización + rechazo) | 8–9 ms |
//! | `ddca_init2` de libddcutil, una vez por proceso | 12–15 ms |
//! | salir de un proceso que cargó libddcutil (sus hilos de vigilancia) | ~490 ms |
//! | una transacción DDC/CI: la norma pide esperar 40–50 ms entre pedido y respuesta | ≥ 40 ms |
//!
//! Lo que ahorraría la biblioteca son los ~8 ms de arrancar un proceso por
//! lectura, contra un piso de 40 ms que impone el protocolo y que ninguna de
//! las dos formas evita. A cambio, enlazarla haría de ddcutil una dependencia
//! dura (la aplicación no arranca sin `libddcutil.so.5`) y metería en el
//! binario `libX11`, `libXrandr`, `libusb`, `libjansson` y `libdrm`, que son
//! sus `NEEDED`, en un escritorio Wayland que no las usa. Con el binario,
//! ddcutil sigue siendo opcional: si falta, los monitores externos se ven «no
//! disponible» y nada más.
//!
//! Lo caro de verdad no era el proceso sino **`ddcutil detect`**, que prueba
//! DDC en cada bus. Eso ya no se hace: el mapa conector → bus sale de sysfs
//! (`drm.rs`), y cada lectura va con `--bus N`, que saltea la detección. El
//! `detect` queda sólo como respaldo para un conector que no publica su bus,
//! una vez y guardado hasta que cambien los monitores.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result};

/// VCP 0x10 es «Brightness» en la norma MCCS.
pub const VCP_BRIGHTNESS: &str = "10";

/// Busca un ejecutable en `PATH` sin lanzar nada: lo que hacía antes
/// `command -v` en un `sh` aparte.
pub fn find_in_path(program: &str, path: Option<OsString>) -> Option<PathBuf> {
    std::env::split_paths(&path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| {
            fs::metadata(candidate)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

pub fn ddcutil_path() -> Option<PathBuf> {
    find_in_path("ddcutil", std::env::var_os("PATH"))
}

/// Si hay algún `/dev/i2c-*`. Sin el módulo `i2c-dev` no hay ninguno.
pub fn i2c_devices_present(dev: &Path) -> bool {
    fs::read_dir(dev)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with("i2c-"))
}

/// Si este usuario puede hablarle al bus. Abrir el dispositivo no hace
/// ninguna transacción: es la misma comprobación que va a hacer ddcutil, sin
/// pagar su arranque para enterarse.
pub fn can_open_bus(dev: &Path, bus: u32) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(dev.join(format!("i2c-{bus}")))
        .map(drop)
}

/// Un monitor que `ddcutil detect` encontró.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub connector: String,
    pub bus: u32,
}

/// Lee `ddcutil detect --brief`. Sólo cuentan los bloques `Display N`: los
/// `Invalid display` son pantallas que no contestan DDC/CI, aunque traigan
/// conector.
///
/// El conector es lo que ata el monitor de ddcutil al resto de la página;
/// cruzar por modelo pondría el deslizador en la pantalla equivocada el día que
/// alguien tenga dos monitores iguales.
pub fn parse_detect(output: &str) -> Vec<Detected> {
    let mut found = Vec::new();
    let mut valid = false;
    let mut bus: Option<u32> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if !line.starts_with(char::is_whitespace) {
            // Un bloque nuevo.
            valid = trimmed
                .strip_prefix("Display ")
                .is_some_and(|n| !n.is_empty() && n.trim().chars().all(|c| c.is_ascii_digit()));
            bus = None;
            continue;
        }

        if !valid {
            continue;
        }

        if let Some(path) = trimmed.strip_prefix("I2C bus:") {
            bus = path
                .trim()
                .strip_prefix("/dev/")
                .and_then(crate::drm::bus_number);
        } else if let Some(connector) = trimmed.strip_prefix("DRM connector:") {
            // "card1-DP-2": el número de tarjeta es de la GPU, no de la pantalla.
            let connector = connector.trim();
            let connector = crate::drm::connector_name(connector).unwrap_or(connector);
            if let Some(bus) = bus {
                found.push(Detected {
                    connector: connector.to_string(),
                    bus,
                });
            }
        }
    }

    found
}

/// `ddcutil getvcp 10 --brief` contesta `VCP 10 C 45 100`: actual y máximo.
pub fn parse_vcp(output: &str) -> Option<(u16, u16)> {
    let line = output
        .lines()
        .find(|line| line.trim_start().starts_with("VCP"))?;
    let fields: Vec<&str> = line.split_whitespace().collect();

    let current: u16 = fields.get(3)?.parse().ok()?;
    let max: u16 = fields.get(4)?.parse().ok()?;
    (max > 0).then_some((current.min(max), max))
}

/// `--bus` saltea la detección de todos los monitores, que es lo lento.
pub fn getvcp_args(bus: u32) -> Vec<String> {
    vec![
        "--bus".into(),
        bus.to_string(),
        "--brief".into(),
        "getvcp".into(),
        VCP_BRIGHTNESS.into(),
    ]
}

/// `--noverify`: ddcutil, por omisión, vuelve a leer el valor después de
/// escribirlo, que es otra transacción de 40 ms o más por cada paso del
/// deslizador.
pub fn setvcp_args(bus: u32, value: u16) -> Vec<String> {
    vec![
        "--bus".into(),
        bus.to_string(),
        "--noverify".into(),
        "setvcp".into(),
        VCP_BRIGHTNESS.into(),
        value.to_string(),
    ]
}

pub fn detect_args() -> Vec<String> {
    vec!["detect".into(), "--brief".into()]
}

/// El valor crudo de DDC para un porcentaje. El máximo de VCP 0x10 casi
/// siempre es 100, pero no siempre: escribir el porcentaje tal cual en un
/// monitor que va a 255 lo deja a media luz.
pub fn to_raw(percent: u8, max: u16) -> u16 {
    ((u32::from(percent.min(100)) * u32::from(max) + 50) / 100) as u16
}

pub fn to_percent(current: u16, max: u16) -> u8 {
    if max == 0 {
        return 0;
    }
    ((u32::from(current) * 100 + u32::from(max) / 2) / u32::from(max)).min(100) as u8
}

/// Corre ddcutil y devuelve su salida. Bloquea: se llama desde
/// `spawn_blocking`, nunca desde el hilo de la interfaz.
pub fn run(program: &Path, args: &[String]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::Ddcutil(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reason = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(Error::Ddcutil(reason));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempTree;

    /// Salida real de `ddcutil detect --brief` con dos monitores.
    const DETECT: &str = r#"Display 1
   I2C bus:             /dev/i2c-4
   DRM connector:       card1-DP-2
   Monitor:             DEL:DELL U2720Q:H7MTP83

Display 2
   I2C bus:             /dev/i2c-5
   DRM connector:       card1-HDMI-A-1
   Monitor:             ACI:ASUS VS239:F8LMQS027497

Invalid display
   I2C bus:             /dev/i2c-8
   Monitor:             :
"#;

    /// La de la máquina de desarrollo: sólo el panel interno, que no habla DDC
    /// y aun así trae conector.
    const DETECT_LAPTOP: &str = r#"Invalid display
   I2C bus:          /dev/i2c-6
   DRM connector:    card1-eDP-1
   drm_connector_id: 111
   Monitor:          BOE::
"#;

    #[test]
    fn asocia_cada_pantalla_con_su_conector_y_su_bus() {
        assert_eq!(
            parse_detect(DETECT),
            vec![
                Detected {
                    connector: "DP-2".into(),
                    bus: 4
                },
                Detected {
                    connector: "HDMI-A-1".into(),
                    bus: 5
                },
            ],
            "el prefijo de la tarjeta es de la GPU y se va"
        );
    }

    /// Dos monitores iguales es justo cuando cruzar por modelo falla.
    #[test]
    fn dos_monitores_iguales_quedan_separados() {
        let output = "Display 1\n   I2C bus: /dev/i2c-3\n   DRM connector: card0-DP-1\n   Monitor: DEL:U2720Q:ABC\n\
                      Display 2\n   I2C bus: /dev/i2c-9\n   DRM connector: card0-DP-2\n   Monitor: DEL:U2720Q:ABC\n";
        assert_eq!(
            parse_detect(output),
            vec![
                Detected {
                    connector: "DP-1".into(),
                    bus: 3
                },
                Detected {
                    connector: "DP-2".into(),
                    bus: 9
                },
            ]
        );
    }

    #[test]
    fn una_pantalla_invalida_no_cuenta_aunque_traiga_conector() {
        assert_eq!(parse_detect(DETECT_LAPTOP), vec![]);
    }

    #[test]
    fn sin_conector_o_sin_bus_se_saltea() {
        assert_eq!(parse_detect("Display 3\n   Monitor: X\n"), vec![]);
        assert_eq!(
            parse_detect("Display 3\n   DRM connector: card0-DP-1\n"),
            vec![]
        );
    }

    #[test]
    fn lee_el_brillo_de_una_respuesta_vcp() {
        assert_eq!(parse_vcp("VCP 10 C 45 100"), Some((45, 100)));
        assert_eq!(parse_vcp("VCP 10 C 50 200"), Some((50, 200)));
        assert_eq!(parse_vcp("VCP 10 C 0 100"), Some((0, 100)));
        assert_eq!(parse_vcp("VCP 10 C 300 255"), Some((255, 255)));
    }

    #[test]
    fn no_inventa_un_brillo() {
        assert_eq!(parse_vcp("DDC communication failed"), None);
        assert_eq!(parse_vcp("VCP 10 C 45"), None);
        assert_eq!(parse_vcp("VCP 10 C 45 0"), None, "sin dividir por cero");
        assert_eq!(parse_vcp("VCP 10 ERR"), None);
    }

    #[test]
    fn los_argumentos_saltean_la_deteccion_y_la_verificacion() {
        assert_eq!(getvcp_args(5), ["--bus", "5", "--brief", "getvcp", "10"]);
        assert_eq!(
            setvcp_args(5, 80),
            ["--bus", "5", "--noverify", "setvcp", "10", "80"]
        );
    }

    #[test]
    fn convierte_contra_el_maximo_del_monitor() {
        assert_eq!(to_raw(50, 100), 50);
        assert_eq!(to_raw(50, 255), 128);
        assert_eq!(to_raw(100, 255), 255);
        assert_eq!(to_raw(0, 255), 0, "en DDC cero es el mínimo, no apagar");
        assert_eq!(to_percent(128, 255), 50);
        assert_eq!(to_percent(45, 100), 45);
        assert_eq!(to_percent(1, 0), 0);
    }

    #[test]
    fn encuentra_ejecutables_en_el_path_sin_lanzar_nada() {
        let tree = TempTree::new();
        let bin = tree.dir("bin");
        let tool = tree.file("bin/ddcutil", "#!/bin/sh\n");
        tree.file("other/ddcutil", "no ejecutable");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();

        let path = std::env::join_paths([tree.path().join("other"), bin.clone()]).unwrap();
        assert_eq!(
            find_in_path("ddcutil", Some(path)),
            Some(bin.join("ddcutil"))
        );
        assert_eq!(find_in_path("wlsunset", Some(bin.into_os_string())), None);
        assert_eq!(find_in_path("ddcutil", None), None);
        assert_eq!(
            find_in_path("ddcutil", Some(OsString::from("relative/bin"))),
            None,
            "una ruta relativa en PATH no se sigue"
        );
    }

    #[test]
    fn ve_si_hay_buses_i2c() {
        let tree = TempTree::new();
        tree.file("null", "");
        assert!(!i2c_devices_present(tree.path()));
        tree.file("i2c-3", "");
        assert!(i2c_devices_present(tree.path()));
        assert!(can_open_bus(tree.path(), 3).is_ok());
        assert!(can_open_bus(tree.path(), 4).is_err());
    }
}
