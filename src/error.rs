use serde::Serializer;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no backlight named '{0}'")]
    UnknownBacklight(String),
    #[error("no DDC/CI display on bus '{0}'")]
    UnknownDisplay(String),
    #[error("ddcutil failed: {0}")]
    Ddcutil(String),
    #[error("invalid night light settings: {0}")]
    InvalidNightLight(String),
    #[error("no configuration directory for this user")]
    NoConfigDir,
}

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn se_serializa_como_texto_para_el_frontend() {
        let json = serde_json::to_string(&Error::UnknownBacklight("acpi_video0".into())).unwrap();
        assert_eq!(json, "\"no backlight named 'acpi_video0'\"");
    }
}
