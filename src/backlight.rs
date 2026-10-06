//! La retroiluminación de los paneles internos, leída de sysfs.
//!
//! Leer es leer dos archivos (0,04 ms medidos); escribir va por logind
//! (`manager.rs`), que le da al usuario de la sesión activa permiso sobre su
//! propia pantalla sin root y sin diálogo de polkit.

use std::fs;
use std::path::Path;

use crate::drm::{self, Connector};

pub const BACKLIGHT_ROOT: &str = "/sys/class/backlight";

/// El campo `type` de sysfs. Cuando hay más de un dispositivo para la misma
/// pantalla, se elige en este orden, que es el que usan también GNOME y
/// systemd-backlight: `firmware` (ACPI) antes que `platform` antes que `raw`
/// (el controlador de la GPU).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BacklightType {
    Firmware,
    Platform,
    Raw,
    Other,
}

impl BacklightType {
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "firmware" => Self::Firmware,
            "platform" => Self::Platform,
            "raw" => Self::Raw,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    pub name: String,
    pub brightness: u32,
    pub max: u32,
    pub kind: BacklightType,
    /// El conector del que cuelga el dispositivo en sysfs (`intel_backlight`
    /// vive adentro de `card1-eDP-1`), si cuelga de uno.
    pub connector: Option<String>,
}

fn read_number(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// El conector a partir del destino del enlace de `/sys/class/backlight/X`:
/// `../../devices/pci0000:00/0000:00:02.0/drm/card1/card1-eDP-1/intel_backlight`.
pub fn connector_of(link_target: &Path) -> Option<String> {
    let parent = link_target.parent()?.file_name()?.to_str()?;
    drm::connector_name(parent).map(str::to_string)
}

/// Todos los dispositivos utilizables, ordenados por nombre.
pub fn list(root: &Path) -> Vec<Backlight> {
    let mut devices: Vec<Backlight> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            let dir = entry.path();
            let max = read_number(&dir.join("max_brightness")).filter(|max| *max > 0)?;
            let brightness = read_number(&dir.join("brightness"))?;
            let kind = fs::read_to_string(dir.join("type"))
                .map(|t| BacklightType::parse(&t))
                .unwrap_or(BacklightType::Other);
            let connector = fs::read_link(&dir).ok().and_then(|t| connector_of(&t));
            Some(Backlight {
                name,
                brightness,
                max,
                kind,
                connector,
            })
        })
        .collect();
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    devices
}

/// Una retroiluminación por pantalla, con el conector al que corresponde.
///
/// Los que cuelgan de un conector son de esa pantalla, sin adivinar. Si
/// ninguno cuelga de uno (`acpi_video0` cuelga de la placa), se toma el mejor
/// por tipo y se le asigna el panel interno conectado: una máquina tiene un
/// solo panel interno, y los externos nunca tienen retroiluminación.
pub fn choose(devices: &[Backlight], connectors: &[Connector]) -> Vec<(Option<String>, Backlight)> {
    let mut linked: Vec<&Backlight> = devices.iter().filter(|d| d.connector.is_some()).collect();

    if linked.is_empty() {
        let Some(best) = devices.iter().min_by_key(|d| (d.kind, d.name.clone())) else {
            return Vec::new();
        };
        let internal = connectors
            .iter()
            .find(|c| drm::is_internal(&c.name))
            .map(|c| c.name.clone());
        return vec![(internal, best.clone())];
    }

    linked.sort_by_key(|d| (d.connector.clone(), d.kind, d.name.clone()));
    linked.dedup_by(|a, b| a.connector == b.connector);
    linked
        .into_iter()
        .map(|d| (d.connector.clone(), d.clone()))
        .collect()
}

/// Porcentaje redondeado, sin pasarse de 100.
pub fn to_percent(value: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }
    ((u64::from(value) * 100 + u64::from(max) / 2) / u64::from(max)).min(100) as u8
}

/// El valor crudo para un porcentaje. Nunca cero: en muchos paneles cero es
/// apagar la retroiluminación, y una pantalla negra no se arregla con el
/// deslizador que ya no se ve.
pub fn to_raw(percent: u8, max: u32) -> u32 {
    let percent = u64::from(percent.min(100));
    ((u64::from(max) * percent + 50) / 100).max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempTree;
    use std::path::PathBuf;

    fn device(name: &str, kind: BacklightType, connector: Option<&str>) -> Backlight {
        Backlight {
            name: name.into(),
            brightness: 50,
            max: 100,
            kind,
            connector: connector.map(str::to_string),
        }
    }

    fn connector(name: &str) -> Connector {
        Connector {
            name: name.into(),
            ddc_bus: None,
        }
    }

    #[test]
    fn el_conector_sale_de_la_carpeta_padre() {
        let target = PathBuf::from(
            "../../devices/pci0000:00/0000:00:02.0/drm/card1/card1-eDP-1/intel_backlight",
        );
        assert_eq!(connector_of(&target), Some("eDP-1".into()));

        let acpi = PathBuf::from("../../devices/pci0000:00/0000:00:02.0/backlight/acpi_video0");
        assert_eq!(connector_of(&acpi), None);
    }

    #[test]
    fn lee_los_dispositivos_y_saltea_los_rotos() {
        let tree = TempTree::new();
        let devices = tree.dir("devices/card1/card1-eDP-1");
        tree.file(
            "devices/card1/card1-eDP-1/intel_backlight/brightness",
            "9600\n",
        );
        tree.file(
            "devices/card1/card1-eDP-1/intel_backlight/max_brightness",
            "19200\n",
        );
        tree.file("devices/card1/card1-eDP-1/intel_backlight/type", "raw\n");
        let class = tree.dir("class");
        std::os::unix::fs::symlink(
            devices.join("intel_backlight"),
            class.join("intel_backlight"),
        )
        .unwrap();
        tree.file("class/broken/brightness", "3\n");
        tree.file("class/broken/max_brightness", "0\n");
        tree.file("class/no_max/brightness", "3\n");

        let found = list(&class);
        assert_eq!(found.len(), 1, "max en cero o ausente no sirve: {found:?}");
        assert_eq!(found[0].name, "intel_backlight");
        assert_eq!(found[0].brightness, 9600);
        assert_eq!(found[0].max, 19200);
        assert_eq!(found[0].kind, BacklightType::Raw);
        assert_eq!(found[0].connector.as_deref(), Some("eDP-1"));
    }

    #[test]
    fn prefiere_el_que_cuelga_del_conector() {
        let devices = vec![
            device("acpi_video0", BacklightType::Firmware, None),
            device("intel_backlight", BacklightType::Raw, Some("eDP-1")),
        ];
        let chosen = choose(&devices, &[connector("eDP-1")]);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].0.as_deref(), Some("eDP-1"));
        assert_eq!(chosen[0].1.name, "intel_backlight");
    }

    #[test]
    fn sin_enlace_elige_por_tipo_y_le_da_el_panel_interno() {
        let devices = vec![
            device("amdgpu_bl0", BacklightType::Raw, None),
            device("acpi_video0", BacklightType::Firmware, None),
        ];
        let chosen = choose(&devices, &[connector("DP-1"), connector("eDP-1")]);
        assert_eq!(chosen, vec![(Some("eDP-1".into()), devices[1].clone())]);

        let desktop = choose(&devices, &[connector("DP-1")]);
        assert_eq!(desktop[0].0, None, "sin panel interno no se inventa uno");
    }

    #[test]
    fn una_por_pantalla() {
        let devices = vec![
            device("a", BacklightType::Raw, Some("eDP-1")),
            device("b", BacklightType::Platform, Some("eDP-1")),
            device("c", BacklightType::Raw, Some("eDP-2")),
        ];
        let chosen: Vec<_> = choose(&devices, &[])
            .into_iter()
            .map(|(c, d)| (c.unwrap(), d.name))
            .collect();
        assert_eq!(
            chosen,
            vec![("eDP-1".into(), "b".into()), ("eDP-2".into(), "c".into())]
        );
    }

    #[test]
    fn sin_dispositivos_no_hay_nada() {
        assert!(choose(&[], &[connector("eDP-1")]).is_empty());
    }

    #[test]
    fn convierte_porcentajes_redondeando() {
        assert_eq!(to_percent(9600, 19200), 50);
        assert_eq!(to_percent(1, 3), 33);
        assert_eq!(to_percent(2, 3), 67, "redondea, no trunca");
        assert_eq!(to_percent(500, 100), 100);
        assert_eq!(to_percent(5, 0), 0, "sin dividir por cero");

        assert_eq!(to_raw(50, 19200), 9600);
        assert_eq!(to_raw(100, 19200), 19200);
        assert_eq!(to_raw(0, 19200), 1, "nunca apaga el panel");
        assert_eq!(to_raw(1, 10), 1);
        assert_eq!(to_raw(200, 255), 255, "más de 100 es 100");
    }

    #[test]
    fn ida_y_vuelta_estable() {
        for max in [7u32, 10, 255, 937, 19200, 120000] {
            for percent in 1..=100u8 {
                let raw = to_raw(percent, max);
                let back = to_percent(raw, max);
                assert!(
                    back.abs_diff(percent) <= (100 / max + 1) as u8,
                    "max {max}: {percent}% → {raw} → {back}%"
                );
            }
        }
    }
}
