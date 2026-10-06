use serde::{Deserialize, Serialize};

/// El evento que se emite cuando cambia el brillo de cualquier pantalla, o
/// cuando cambia qué pantallas se pueden atenuar (se conectó un monitor, o
/// terminó de averiguarse cuáles responden por DDC/CI).
pub const BRIGHTNESS_EVENT: &str = "display-brightness-changed";

/// Por dónde se llega al brillo de una pantalla.
///
/// Un panel de notebook tiene una retroiluminación que maneja el kernel. Un
/// monitor externo no tiene ninguna: la única forma de tocar su brillo es
/// DDC/CI, un canal de control que viaja por el cable de video y contesta por
/// i2c. Son mecanismos distintos, y por eso un solo deslizador para «el brillo»
/// no puede andar en un escritorio con dos pantallas.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BrightnessKind {
    /// `/sys/class/backlight`, escrito por logind.
    Backlight,
    /// DDC/CI por i2c.
    Ddc,
}

/// El brillo de una pantalla.
///
/// Todo lo que sale hacia el frontend va en camelCase, igual que en el resto de
/// los complementos del taller y que en `guest-js/index.ts`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MonitorBrightness {
    /// El conector DRM (`eDP-1`, `DP-2`), para cruzarlo con la lista de
    /// monitores. Puede faltar en un panel cuya retroiluminación no cuelga de
    /// ningún conector y que no tiene uno interno a la vista.
    pub output: Option<String>,
    pub kind: BrightnessKind,
    /// Lo que hay que devolver en `set_brightness`: el nombre de la
    /// retroiluminación, o el número de bus i2c del monitor.
    pub handle: String,
    pub percent: u8,
}

/// En qué está la búsqueda de monitores externos.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DdcState {
    /// Lo que hay en `monitors` es todo lo que se sabe.
    Ready,
    /// Se está averiguando en segundo plano; el resultado llega con
    /// [`BRIGHTNESS_EVENT`]. Nunca se espera en el camino de abrir una
    /// ventana.
    Detecting,
    /// Hay monitores externos pero no hay forma de hablarles; `reason` dice
    /// por qué.
    Unavailable,
}

/// Por qué no se puede usar DDC/CI. Son códigos y no frases: el texto lo pone
/// cada aplicación, traducido.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DdcUnavailableReason {
    /// `ddcutil` no está instalado.
    NotInstalled,
    /// No hay ningún `/dev/i2c-*`: falta el módulo `i2c-dev`.
    NoI2cDev,
    /// Hay buses pero este usuario no puede abrirlos (grupo `i2c` o la regla
    /// de udev de ddcutil).
    NoPermission,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DdcStatus {
    pub state: DdcState,
    pub reason: Option<DdcUnavailableReason>,
    /// Los conectores de monitores externos que no contestan por DDC/CI (lo
    /// tienen apagado en su menú, o no lo soportan). Se muestran «no
    /// disponible», no se esconden.
    pub unsupported: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrightnessReport {
    pub monitors: Vec<MonitorBrightness>,
    pub ddc: DdcStatus,
}

/// Cómo decide la luz nocturna cuándo es de noche.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NightLightMode {
    /// Horarios fijos.
    #[default]
    Manual,
    /// Amanecer y atardecer según la latitud y la longitud.
    Location,
}

/// La configuración de la luz nocturna (`wlsunset`).
///
/// Es sólo la configuración: encender y apagar el servicio es de quien la usa
/// (vasak-desktop#178).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NightLightConfig {
    pub mode: NightLightMode,
    /// Kelvin de día (`wlsunset -T`).
    pub day_temperature: u32,
    /// Kelvin de noche (`wlsunset -t`); menor que la de día.
    pub night_temperature: u32,
    /// `HH:MM` en que empieza el día, en modo manual (`-S`).
    pub sunrise: String,
    /// `HH:MM` en que empieza la noche, en modo manual (`-s`).
    pub sunset: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

impl Default for NightLightConfig {
    fn default() -> Self {
        Self {
            mode: NightLightMode::Manual,
            day_temperature: 6500,
            night_temperature: 4000,
            sunrise: "07:00".to_string(),
            sunset: "20:00".to_string(),
            latitude: None,
            longitude: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NightLight {
    /// Falso cuando `wlsunset` no está instalado: la interfaz lo explica en
    /// vez de fallar.
    pub available: bool,
    /// Si la configuración ya se guardó alguna vez. Sin guardar, `config` son
    /// los valores por omisión.
    pub configured: bool,
    pub config: NightLightConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(value: serde_json::Value) -> Vec<String> {
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Las claves tienen que ser las que declara `guest-js/index.ts`.
    #[test]
    fn las_claves_salen_en_camel_case() {
        let report = BrightnessReport {
            monitors: vec![MonitorBrightness {
                output: Some("eDP-1".into()),
                kind: BrightnessKind::Backlight,
                handle: "intel_backlight".into(),
                percent: 40,
            }],
            ddc: DdcStatus {
                state: DdcState::Unavailable,
                reason: Some(DdcUnavailableReason::NoI2cDev),
                unsupported: vec![],
            },
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(keys(json.clone()), vec!["ddc", "monitors"]);
        assert_eq!(
            keys(json["monitors"][0].clone()),
            vec!["handle", "kind", "output", "percent"]
        );
        assert_eq!(
            keys(json["ddc"].clone()),
            vec!["reason", "state", "unsupported"]
        );
        assert_eq!(json["monitors"][0]["kind"], "backlight");
        assert_eq!(json["ddc"]["state"], "unavailable");
        assert_eq!(json["ddc"]["reason"], "no-i2c-dev");

        let night = serde_json::to_value(NightLight {
            available: true,
            configured: false,
            config: NightLightConfig::default(),
        })
        .unwrap();
        assert_eq!(
            keys(night.clone()),
            vec!["available", "config", "configured"]
        );
        assert_eq!(
            keys(night["config"].clone()),
            vec![
                "dayTemperature",
                "latitude",
                "longitude",
                "mode",
                "nightTemperature",
                "sunrise",
                "sunset"
            ]
        );
        assert_eq!(night["config"]["mode"], "manual");
    }

    #[test]
    fn los_codigos_de_razon_son_los_del_binding() {
        for (reason, code) in [
            (DdcUnavailableReason::NotInstalled, "not-installed"),
            (DdcUnavailableReason::NoI2cDev, "no-i2c-dev"),
            (DdcUnavailableReason::NoPermission, "no-permission"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), code);
        }
    }
}
