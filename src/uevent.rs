//! Los avisos del kernel sobre pantallas, sin sondeo.
//!
//! El kernel publica un uevent por netlink cuando cambia el brillo de una
//! retroiluminación (también cuando lo cambia una tecla o otro programa: lo
//! dispara la escritura en sysfs, que es lo que hace logind) y cuando se
//! conecta o desconecta un monitor (`SUBSYSTEM=drm`, `HOTPLUG=1`). Un hilo
//! dormido en `recv` los espera: no cuesta nada mientras no pasa nada.
//!
//! Se lee el grupo del kernel y no el de udev para no depender de `libudev`.
//! Sólo se aceptan mensajes del kernel (puerto 0): el grupo no admite que un
//! proceso sin privilegios escriba en él, pero mirar el remitente es gratis.

use std::io;
use std::thread;

use rustix::net::{
    bind, netlink, recvfrom, socket_with, AddressFamily, RecvFlags, SocketFlags, SocketType,
};

/// Lo que le importa al plugin de un uevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uevent {
    /// Cambió el brillo de alguna retroiluminación.
    Backlight,
    /// Se conectó, desconectó o cambió un monitor.
    DrmHotplug,
    /// Se perdieron mensajes (se llenó el búfer del socket): no se sabe qué
    /// cambió, así que hay que suponer que cambió todo.
    Lost,
}

/// Lee un mensaje del kernel: `accion@ruta\0CLAVE=valor\0…`.
pub fn parse(message: &[u8]) -> Option<Uevent> {
    let mut fields = message.split(|b| *b == 0).filter(|f| !f.is_empty());
    let header = fields.next()?;
    // Los de udev empiezan con «libudev» y tienen otro formato; por este
    // grupo no deberían llegar, pero si llegaran no son del kernel.
    if !header.contains(&b'@') {
        return None;
    }

    let mut action = None;
    let mut subsystem = None;
    let mut hotplug = false;
    for field in fields {
        let Some(eq) = field.iter().position(|b| *b == b'=') else {
            continue;
        };
        let (key, value) = (&field[..eq], &field[eq + 1..]);
        match key {
            b"ACTION" => action = Some(value),
            b"SUBSYSTEM" => subsystem = Some(value),
            b"HOTPLUG" => hotplug = value == b"1",
            _ => {}
        }
    }

    match (subsystem?, action?) {
        (b"backlight", b"change") => Some(Uevent::Backlight),
        (b"drm", b"change") if hotplug => Some(Uevent::DrmHotplug),
        // Una GPU que aparece o se va (eGPU, cambio de controlador).
        (b"drm", b"add" | b"remove") => Some(Uevent::DrmHotplug),
        _ => None,
    }
}

/// Abre el socket y lanza el hilo que lo escucha. Si el socket no se puede
/// abrir (un contenedor sin netlink), devuelve el error y el plugin sigue sin
/// avisos: el brillo se sigue leyendo bien, sólo que nadie avisa de cambios
/// ajenos.
pub fn spawn<F>(handler: F) -> io::Result<()>
where
    F: Fn(Uevent) + Send + 'static,
{
    let socket = socket_with(
        AddressFamily::NETLINK,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC,
        Some(netlink::KOBJECT_UEVENT),
    )?;
    // Grupo 1: los uevents del kernel.
    bind(&socket, &netlink::SocketAddrNetlink::new(0, 1))?;

    thread::Builder::new()
        .name("display-manager-uevent".into())
        .spawn(move || {
            let mut buffer = vec![0u8; 16 * 1024];
            loop {
                match recvfrom(&socket, &mut buffer[..], RecvFlags::empty()) {
                    Ok((_, length, from)) => {
                        let from_kernel = from
                            .and_then(|a| netlink::SocketAddrNetlink::try_from(a).ok())
                            .is_some_and(|a| a.pid() == 0);
                        if !from_kernel {
                            continue;
                        }
                        let length = length.min(buffer.len());
                        if let Some(event) = parse(&buffer[..length]) {
                            handler(event);
                        }
                    }
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(rustix::io::Errno::NOBUFS) => handler(Uevent::Lost),
                    Err(e) => {
                        log::warn!("display-manager: se cortaron los uevents: {e}");
                        break;
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(parts: &[&str]) -> Vec<u8> {
        parts.join("\0").into_bytes()
    }

    #[test]
    fn un_cambio_de_brillo() {
        // Lo que publicó el kernel en la máquina de desarrollo al llamar a
        // `SetBrightness` de logind.
        let msg = message(&[
            "change@/devices/pci0000:00/0000:00:02.0/drm/card1/card1-eDP-1/intel_backlight",
            "ACTION=change",
            "DEVPATH=/devices/pci0000:00/0000:00:02.0/drm/card1/card1-eDP-1/intel_backlight",
            "SUBSYSTEM=backlight",
            "SOURCE=sysfs",
            "SEQNUM=6639",
        ]);
        assert_eq!(parse(&msg), Some(Uevent::Backlight));
    }

    #[test]
    fn un_monitor_que_se_conecta() {
        let msg = message(&[
            "change@/devices/pci0000:00/0000:00:02.0/drm/card1",
            "ACTION=change",
            "DEVPATH=/devices/pci0000:00/0000:00:02.0/drm/card1",
            "SUBSYSTEM=drm",
            "HOTPLUG=1",
            "CONNECTOR=111",
            "DEVNAME=dri/card1",
            "SEQNUM=7001",
            "",
        ]);
        assert_eq!(parse(&msg), Some(Uevent::DrmHotplug));
    }

    #[test]
    fn un_cambio_de_drm_sin_hotplug_no_es_un_monitor() {
        let msg = message(&[
            "change@/devices/pci0000:00/0000:00:02.0/drm/card1",
            "ACTION=change",
            "SUBSYSTEM=drm",
            "HOTPLUG=0",
        ]);
        assert_eq!(parse(&msg), None);
    }

    #[test]
    fn una_gpu_que_aparece() {
        let msg = message(&["add@/devices/x/drm/card2", "ACTION=add", "SUBSYSTEM=drm"]);
        assert_eq!(parse(&msg), Some(Uevent::DrmHotplug));
    }

    #[test]
    fn lo_demas_no_interesa() {
        let usb = message(&["add@/devices/usb1", "ACTION=add", "SUBSYSTEM=usb"]);
        assert_eq!(parse(&usb), None);
        let power = message(&[
            "change@/devices/BAT0",
            "ACTION=change",
            "SUBSYSTEM=power_supply",
        ]);
        assert_eq!(parse(&power), None);
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"libudev\0\xfe\xed"), None);
        assert_eq!(
            parse(&message(&["change@/x", "SUBSYSTEM=backlight"])),
            None,
            "sin acción no se adivina"
        );
    }
}
