//! La configuración de la luz nocturna (`wlsunset`).
//!
//! Vive en la unidad de systemd del usuario que corre `wlsunset`
//! (`~/.config/systemd/user/vasak-nightlight.service`): la línea `ExecStart`
//! es la única fuente de verdad, y la configuración se lee de vuelta de ahí en
//! vez de duplicarse en otro archivo. Es el mismo archivo que escribía
//! vasak-settings, así que lo que el usuario ya tenía configurado se sigue
//! leyendo igual.
//!
//! Esto es sólo la configuración. Encender, apagar y recargar el servicio es de
//! quien la usa (vasak-desktop#178); [`wlsunset_args`] está público para eso.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ddc::find_in_path;
use crate::models::{NightLight, NightLightConfig, NightLightMode};
use crate::{Error, Result};

pub const UNIT_NAME: &str = "vasak-nightlight.service";

pub const MIN_TEMPERATURE: u32 = 1000;
pub const MAX_TEMPERATURE: u32 = 10000;

/// Dónde está la unidad, respetando `XDG_CONFIG_HOME`.
pub fn unit_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .ok_or(Error::NoConfigDir)?
        .join("systemd/user")
        .join(UNIT_NAME))
}

pub fn wlsunset_available() -> bool {
    find_in_path("wlsunset", std::env::var_os("PATH")).is_some()
}

/// `7:5` no; `7:05` y `07:05` sí, y se devuelve siempre como `07:05`.
pub fn parse_time(text: &str) -> Option<String> {
    let (hours, minutes) = text.trim().split_once(':')?;
    if hours.is_empty() || hours.len() > 2 || minutes.len() != 2 {
        return None;
    }
    let hours: u8 = hours.parse().ok()?;
    let minutes: u8 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then(|| format!("{hours:02}:{minutes:02}"))
}

fn coordinate(value: Option<f64>, limit: f64, name: &str) -> Result<Option<f64>> {
    match value {
        None => Ok(None),
        Some(v) if v.is_finite() && v.abs() <= limit => Ok(Some(v)),
        Some(v) => Err(Error::InvalidNightLight(format!(
            "{name} out of range: {v}"
        ))),
    }
}

/// Deja la configuración lista para escribirse, o dice qué está mal.
///
/// Esto no es sólo prolijidad: los valores terminan en la línea `ExecStart`
/// de una unidad de systemd, y un texto con un espacio o un salto de línea
/// agregaría argumentos o directivas. Por eso las horas se reescriben y las
/// coordenadas son números, no texto.
pub fn validate(config: &NightLightConfig) -> Result<NightLightConfig> {
    let day = config
        .day_temperature
        .clamp(MIN_TEMPERATURE, MAX_TEMPERATURE);
    let night = config
        .night_temperature
        .clamp(MIN_TEMPERATURE, MAX_TEMPERATURE);
    if night >= day {
        return Err(Error::InvalidNightLight(format!(
            "night temperature ({night} K) must be lower than day temperature ({day} K)"
        )));
    }

    let sunrise = parse_time(&config.sunrise).ok_or_else(|| {
        Error::InvalidNightLight(format!("invalid sunrise time: {:?}", config.sunrise))
    })?;
    let sunset = parse_time(&config.sunset).ok_or_else(|| {
        Error::InvalidNightLight(format!("invalid sunset time: {:?}", config.sunset))
    })?;

    Ok(NightLightConfig {
        mode: config.mode,
        day_temperature: day,
        night_temperature: night,
        sunrise,
        sunset,
        latitude: coordinate(config.latitude, 90.0, "latitude")?,
        longitude: coordinate(config.longitude, 180.0, "longitude")?,
    })
}

/// Los argumentos de `wlsunset` para una configuración ya validada.
///
/// En modo ubicación sin las dos coordenadas se usan los horarios: una
/// ubicación a medias no puede producir un comando que `wlsunset` rechace.
pub fn wlsunset_args(config: &NightLightConfig) -> Vec<String> {
    let mut args = vec![
        "-t".to_string(),
        config.night_temperature.to_string(),
        "-T".to_string(),
        config.day_temperature.to_string(),
    ];

    match (config.mode, config.latitude, config.longitude) {
        (NightLightMode::Location, Some(lat), Some(lon)) => {
            args.extend(["-l".into(), lat.to_string(), "-L".into(), lon.to_string()]);
        }
        _ => {
            args.extend([
                "-S".into(),
                config.sunrise.clone(),
                "-s".into(),
                config.sunset.clone(),
            ]);
        }
    }
    args
}

pub fn render_unit(config: &NightLightConfig) -> String {
    format!(
        "# Generado por VasakOS (tauri-plugin-display-manager). Los cambios manuales se sobrescriben.\n\
         [Unit]\n\
         Description=Luz nocturna de VasakOS (wlsunset)\n\
         PartOf=graphical-session.target\n\
         After=graphical-session.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=wlsunset {}\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n",
        wlsunset_args(config).join(" ")
    )
}

/// Lee la configuración de la línea `ExecStart`. Lo que no se entiende queda
/// con el valor por omisión, en vez de fallar: es un archivo que el usuario
/// pudo haber tocado.
pub fn parse_unit(content: &str) -> NightLightConfig {
    let mut config = NightLightConfig::default();

    let Some(exec) = content
        .lines()
        .map(str::trim_start)
        .find_map(|line| line.strip_prefix("ExecStart="))
    else {
        return config;
    };

    let tokens: Vec<&str> = exec.split_whitespace().collect();
    let mut location = (false, false);
    for pair in tokens.windows(2) {
        let (flag, value) = (pair[0], pair[1]);
        match flag {
            "-t" => {
                if let Ok(v) = value.parse() {
                    config.night_temperature = v;
                }
            }
            "-T" => {
                if let Ok(v) = value.parse() {
                    config.day_temperature = v;
                }
            }
            "-S" => {
                if let Some(t) = parse_time(value) {
                    config.sunrise = t;
                }
            }
            "-s" => {
                if let Some(t) = parse_time(value) {
                    config.sunset = t;
                }
            }
            "-l" => {
                config.latitude = value.parse().ok().filter(|v: &f64| v.is_finite());
                location.0 = true;
            }
            "-L" => {
                config.longitude = value.parse().ok().filter(|v: &f64| v.is_finite());
                location.1 = true;
            }
            _ => {}
        }
    }
    if location.0 || location.1 {
        config.mode = NightLightMode::Location;
    }
    config
}

/// La configuración guardada, o la de omisión si todavía no hay.
pub fn read(path: &Path) -> NightLight {
    let (configured, config) = match fs::read_to_string(path) {
        Ok(content) => (true, parse_unit(&content)),
        Err(_) => (false, NightLightConfig::default()),
    };
    NightLight {
        available: wlsunset_available(),
        configured,
        config,
    }
}

/// Valida y guarda. Escribe en un temporal y lo renombra: una unidad a medio
/// escribir haría que systemd no levante la luz nocturna en el próximo inicio.
pub fn write(path: &Path, config: &NightLightConfig) -> Result<NightLight> {
    let config = validate(config)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("service.tmp");
    fs::write(&temporary, render_unit(&config))?;
    fs::rename(&temporary, path)?;

    Ok(read(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempTree;

    fn manual(night: u32, day: u32, sunrise: &str, sunset: &str) -> NightLightConfig {
        NightLightConfig {
            mode: NightLightMode::Manual,
            day_temperature: day,
            night_temperature: night,
            sunrise: sunrise.into(),
            sunset: sunset.into(),
            latitude: None,
            longitude: None,
        }
    }

    #[test]
    fn genera_los_argumentos_con_horario_fijo() {
        let config = manual(3500, 6500, "07:30", "19:45");
        assert_eq!(
            wlsunset_args(&config),
            ["-t", "3500", "-T", "6500", "-S", "07:30", "-s", "19:45"]
        );
        let unit = render_unit(&config);
        assert!(unit.contains("ExecStart=wlsunset -t 3500 -T 6500 -S 07:30 -s 19:45\n"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
    }

    #[test]
    fn genera_los_argumentos_por_ubicacion() {
        let config = NightLightConfig {
            mode: NightLightMode::Location,
            latitude: Some(-34.6),
            longitude: Some(-58.4),
            ..NightLightConfig::default()
        };
        let args = wlsunset_args(&config);
        assert_eq!(&args[4..], ["-l", "-34.6", "-L", "-58.4"]);
        assert!(!args.contains(&"-S".to_string()), "sin horarios fijos");
    }

    #[test]
    fn una_ubicacion_a_medias_usa_los_horarios() {
        let config = NightLightConfig {
            mode: NightLightMode::Location,
            latitude: Some(-34.6),
            longitude: None,
            ..NightLightConfig::default()
        };
        assert!(wlsunset_args(&config).contains(&"-S".to_string()));
    }

    #[test]
    fn ida_y_vuelta_por_la_unidad() {
        let config = manual(3000, 6000, "08:00", "21:00");
        assert_eq!(parse_unit(&render_unit(&config)), config);

        let location = NightLightConfig {
            mode: NightLightMode::Location,
            latitude: Some(40.4),
            longitude: Some(-3.7),
            ..NightLightConfig::default()
        };
        let parsed = parse_unit(&render_unit(&location));
        assert_eq!(parsed.mode, NightLightMode::Location);
        assert_eq!(parsed.latitude, Some(40.4));
        assert_eq!(parsed.longitude, Some(-3.7));
    }

    /// Lo que escribía vasak-settings antes de este plugin se sigue leyendo.
    #[test]
    fn lee_la_unidad_que_escribia_vasak_settings() {
        let old = "# Generado por vasak-settings. Los cambios manuales se sobrescriben.\n\
                   [Unit]\nDescription=Luz nocturna de VasakOS (wlsunset)\n\n\
                   [Service]\nType=simple\nExecStart=wlsunset -t 3500 -T 6500 -S 07:30 -s 19:45\n";
        assert_eq!(parse_unit(old), manual(3500, 6500, "07:30", "19:45"));
    }

    #[test]
    fn lo_que_no_se_entiende_queda_por_omision() {
        assert_eq!(parse_unit(""), NightLightConfig::default());
        let odd = parse_unit("ExecStart=wlsunset -t caliente -S 25:00 -l norte");
        assert_eq!(odd.night_temperature, 4000);
        assert_eq!(odd.sunrise, "07:00");
        assert_eq!(odd.mode, NightLightMode::Location);
        assert_eq!(odd.latitude, None);
    }

    #[test]
    fn las_temperaturas_se_acotan_y_la_noche_es_mas_calida() {
        let clamped = validate(&manual(10, 99999, "07:00", "20:00")).unwrap();
        assert_eq!(clamped.night_temperature, MIN_TEMPERATURE);
        assert_eq!(clamped.day_temperature, MAX_TEMPERATURE);

        assert!(
            validate(&manual(6500, 6500, "07:00", "20:00")).is_err(),
            "wlsunset rechaza una noche igual o más fría que el día"
        );
        assert!(validate(&manual(7000, 6500, "07:00", "20:00")).is_err());
    }

    #[test]
    fn las_horas_se_reescriben_y_no_dejan_pasar_texto() {
        assert_eq!(parse_time("7:05"), Some("07:05".into()));
        assert_eq!(parse_time(" 07:05 "), Some("07:05".into()));
        for bad in [
            "24:00",
            "07:60",
            "7:5",
            "07",
            "",
            "07:00 -o x",
            "07:00\nExecStartPre=x",
            "a:bc",
            "007:00",
        ] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
        let injected = manual(3500, 6500, "07:00\nExecStartPre=/bin/sh", "20:00");
        assert!(validate(&injected).is_err());
    }

    #[test]
    fn las_coordenadas_tienen_que_existir() {
        let with = |lat, lon| NightLightConfig {
            mode: NightLightMode::Location,
            latitude: lat,
            longitude: lon,
            ..NightLightConfig::default()
        };
        assert!(validate(&with(Some(-34.6), Some(-58.4))).is_ok());
        assert!(validate(&with(Some(91.0), Some(0.0))).is_err());
        assert!(validate(&with(Some(0.0), Some(-180.5))).is_err());
        assert!(validate(&with(Some(f64::NAN), Some(0.0))).is_err());
        assert!(
            validate(&with(None, None)).is_ok(),
            "puede faltar: usa horarios"
        );
    }

    #[test]
    fn escribe_y_vuelve_a_leer() {
        let tree = TempTree::new();
        let path = tree.path().join("systemd/user").join(UNIT_NAME);

        let empty = read(&path);
        assert!(!empty.configured);
        assert_eq!(empty.config, NightLightConfig::default());

        let saved = write(&path, &manual(3200, 6200, "6:45", "21:15")).unwrap();
        assert!(saved.configured);
        assert_eq!(saved.config, manual(3200, 6200, "06:45", "21:15"));
        assert!(
            !path.with_extension("service.tmp").exists(),
            "sin temporales"
        );

        assert!(write(&path, &manual(6200, 3200, "06:45", "21:15")).is_err());
        assert_eq!(
            read(&path).config.night_temperature,
            3200,
            "una configuración inválida no pisa la que había"
        );
    }

    #[test]
    fn la_unidad_vive_en_la_carpeta_de_systemd_del_usuario() {
        if let Ok(path) = unit_path() {
            assert!(path.ends_with("systemd/user/vasak-nightlight.service"));
        }
    }
}
