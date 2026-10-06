//! Los conectores de video, leídos de `/sys/class/drm`.
//!
//! Es lo que reemplaza a `ddcutil detect` para saber qué bus i2c le habla a
//! qué monitor: el kernel publica, para cada conector, un enlace `ddc` al
//! adaptador i2c de ese cable. Leerlo cuesta lo que cuesta listar una carpeta
//! (0,15 ms medidos en la máquina de desarrollo, contra un `ddcutil detect`
//! que prueba DDC en cada bus).

use std::fs;
use std::path::Path;

pub const DRM_ROOT: &str = "/sys/class/drm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    /// `eDP-1`, `DP-2`, `HDMI-A-1`: sin el prefijo `cardN-`, que es de la GPU.
    pub name: String,
    /// El número del bus i2c del cable (`/dev/i2c-N`), si el kernel lo publica.
    pub ddc_bus: Option<u32>,
}

/// Un panel interno es eDP, LVDS o DSI. Los externos nunca tienen
/// retroiluminación, y los internos no hablan DDC/CI: probarlos por i2c tarda
/// segundos en fallar (ddcutil tardó 3,4 s en el eDP de la máquina de
/// desarrollo).
pub fn is_internal(connector: &str) -> bool {
    let name = connector.to_ascii_lowercase();
    name.starts_with("edp") || name.starts_with("lvds") || name.starts_with("dsi")
}

/// `card1-DP-2` → `DP-2`. Lo que no tiene la forma de un conector no es uno
/// (`card1`, `renderD128`, `version`).
pub fn connector_name(entry: &str) -> Option<&str> {
    let (card, name) = entry.split_once('-')?;
    let number = card.strip_prefix("card")?;
    (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) && !name.is_empty())
        .then_some(name)
}

/// `i2c-7` → 7.
pub fn bus_number(name: &str) -> Option<u32> {
    name.strip_prefix("i2c-")?.parse().ok()
}

fn ddc_bus(dir: &Path) -> Option<u32> {
    // El enlace `ddc` lo publican los controladores que exponen el bus del
    // cable (HDMI, DVI, VGA, y DP en la mayoría).
    if let Some(bus) = fs::read_link(dir.join("ddc"))
        .ok()
        .and_then(|target| target.file_name()?.to_str().and_then(bus_number))
    {
        return Some(bus);
    }
    // DisplayPort sin `ddc`: el canal AUX aparece como un `i2c-N` adentro.
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find_map(|entry| entry.file_name().to_str().and_then(bus_number))
}

/// Los conectores con una pantalla enchufada, ordenados por nombre.
pub fn connected(root: &Path) -> Vec<Connector> {
    let mut found: Vec<Connector> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let name = connector_name(file_name.to_str()?)?.to_string();
            let dir = entry.path();
            let status = fs::read_to_string(dir.join("status")).ok()?;
            (status.trim() == "connected").then(|| Connector {
                name,
                ddc_bus: ddc_bus(&dir),
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempTree;

    #[test]
    fn reconoce_los_paneles_internos() {
        for name in ["eDP-1", "LVDS-1", "DSI-1", "edp-2"] {
            assert!(is_internal(name), "{name}");
        }
        for name in ["DP-1", "HDMI-A-1", "DVI-D-1", "VGA-1"] {
            assert!(!is_internal(name), "{name}");
        }
    }

    #[test]
    fn saca_el_prefijo_de_la_gpu() {
        assert_eq!(connector_name("card1-DP-2"), Some("DP-2"));
        assert_eq!(connector_name("card0-HDMI-A-1"), Some("HDMI-A-1"));
        assert_eq!(connector_name("card1"), None);
        assert_eq!(connector_name("renderD128"), None);
        assert_eq!(connector_name("cardX-DP-1"), None);
        assert_eq!(connector_name("card-DP-1"), None);
        assert_eq!(connector_name("card1-"), None);
    }

    #[test]
    fn lee_el_bus_del_enlace_ddc_o_del_canal_aux() {
        let tree = TempTree::new();
        tree.file("card1-eDP-1/status", "connected\n");
        tree.dir("card1-eDP-1/i2c-6");
        tree.file("card1-HDMI-A-1/status", "connected\n");
        tree.symlink("card1-HDMI-A-1/ddc", "../../../i2c-2");
        tree.file("card1-DP-1/status", "disconnected\n");
        tree.symlink("card1-DP-1/ddc", "i2c-7");
        tree.file("card1-DP-2/status", "connected\n");
        tree.file("version", "drm 1.1.0\n");

        assert_eq!(
            connected(tree.path()),
            vec![
                Connector {
                    name: "DP-2".into(),
                    ddc_bus: None
                },
                Connector {
                    name: "HDMI-A-1".into(),
                    ddc_bus: Some(2)
                },
                Connector {
                    name: "eDP-1".into(),
                    ddc_bus: Some(6)
                },
            ],
            "el desconectado no aparece, y uno sin bus aparece sin bus"
        );
    }

    #[test]
    fn sin_carpeta_no_hay_conectores() {
        assert!(connected(Path::new("/nonexistent/drm")).is_empty());
    }
}
