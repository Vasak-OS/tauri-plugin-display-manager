# tauri-plugin-display-manager

Plugin de Tauri para el **brillo por monitor** —el panel interno por sysfs y
logind, los monitores externos por DDC/CI— y la **configuración de la luz
nocturna** (`wlsunset`).

Es parte de VasakOS: lo usan `vasak-settings` y el centro de control de
`vasak-desktop`. Sólo Linux.

## Cómo funciona, y por qué es barato

| | cómo | costo medido |
|---|---|---|
| Leer el panel interno | `/sys/class/backlight/*/{brightness,max_brightness}` | 0,04 ms |
| Escribir el panel interno | `SetBrightness` de logind por D-Bus (sin root ni polkit) | un viaje por el bus |
| Saber qué bus i2c es de qué monitor | el enlace `ddc` de `/sys/class/drm/cardN-*` | 0,15 ms |
| Leer o escribir un monitor externo | `ddcutil --bus N getvcp/setvcp 10` en segundo plano | ≥ 40 ms (lo pide la norma DDC/CI) + ~8 ms del proceso |
| Enterarse de un cambio | uevents del kernel por netlink: brillo (`SUBSYSTEM=backlight`) y monitores (`SUBSYSTEM=drm`, `HOTPLUG=1`) | un hilo dormido |
| `get_brightness` completo | panel desde sysfs + externos desde lo guardado | **0,044 ms** promedio (compilado en release) |

- **`get_brightness` nunca espera a DDC/CI.** Devuelve lo guardado. La primera
  vez (o cuando cambian los monitores) los externos vienen con
  `ddc.state: "detecting"` y llegan después por el evento
  `display-brightness-changed`.
- **Sin `ddcutil detect`.** Era lo lento: prueba DDC en todos los buses. El
  mapa conector → bus sale de sysfs, y cada lectura va con `--bus N`, que
  saltea la detección. `detect` queda como respaldo para un conector que no
  publica su bus, una vez, y guardado hasta el próximo cambio de monitores.
- **Los paneles internos no se prueban por DDC.** No lo hablan, y probarlo
  tarda segundos en fallar (3,4 s medidos con ddcutil en un eDP).
- **Sin sondeo.** El kernel avisa cuando cambia el brillo —también desde una
  tecla u otro programa— y cuando se conecta o desconecta un monitor; eso
  invalida lo guardado. El brillo de un monitor externo cambiado desde sus
  propios botones no avisa: se relee si lo guardado tiene más de 60 s cuando
  alguien pregunta, en segundo plano, o con `refresh_brightness`.
- **El deslizador no hace cola.** Arrastrarlo pide decenas de valores por
  segundo; a un monitor externo se le escribe sólo el último.
- **Lo que no hay se ve «no disponible»,** con un código: `not-installed`
  (falta ddcutil), `no-i2c-dev` (falta el módulo), `no-permission` (grupo
  `i2c` o la regla de udev de ddcutil). Un monitor que no contesta DDC/CI va
  en `ddc.unsupported`. Sin monitores externos no se dice nada: no hace
  falta ddcutil para una notebook sola.

### Por qué `ddcutil` y no `libddcutil`

Medido con ddcutil 3.0.2 / libddcutil 5.6.2:

| | costo |
|---|---|
| arrancar `ddcutil` y salir | 4–5 ms |
| `ddcutil --bus N getvcp 10` sin monitor (arranque + inicialización + rechazo) | 8–9 ms |
| `ddca_init2` de libddcutil, una vez por proceso | 12–15 ms |
| salir de un proceso que cargó libddcutil | ~490 ms |
| una transacción DDC/CI | ≥ 40 ms |

La biblioteca ahorraría unos 8 ms por lectura contra un piso de 40 ms que no
evita nadie, y a cambio haría de ddcutil una dependencia dura (la aplicación no
arranca sin `libddcutil.so.5`) y metería `libX11`, `libXrandr`, `libusb`,
`libjansson` y `libdrm` —sus `NEEDED`— en aplicaciones Wayland que no las
usan. Con el binario, ddcutil es opcional.

## Luz nocturna

Sólo la configuración: temperatura de día y de noche, y horario fijo o por
ubicación. Vive en la línea `ExecStart` de
`~/.config/systemd/user/vasak-nightlight.service` —la misma unidad que ya
escribía vasak-settings, así que lo configurado se sigue leyendo—. Encender,
apagar y recargar el servicio es de quien lo usa; `night_light::wlsunset_args`
está público para eso.

Los valores se validan antes de escribirse: terminan en una unidad de systemd,
y un texto con un salto de línea agregaría directivas. Las horas se reescriben
como `HH:MM` y las coordenadas son números.

## Instalación

```toml
# src-tauri/Cargo.toml
[dependencies]
tauri-plugin-display-manager = "2"
```

```sh
bun add @vasakgroup/plugin-display-manager
```

```rust
tauri::Builder::default()
    // Una aplicación que se abre a demanda:
    .plugin(tauri_plugin_display_manager::init())
    // El escritorio, que vive toda la sesión, busca los externos al arrancar:
    // .plugin(tauri_plugin_display_manager::Builder::new().prefetch_ddc(true).build())
```

```json
"permissions": ["display-manager:default"]
```

## Uso

```ts
import {
  getBrightness,
  setBrightness,
  onBrightnessChanged,
  getNightLight,
  setNightLight,
} from '@vasakgroup/plugin-display-manager';

const report = await getBrightness();
// { monitors: [{ output: 'eDP-1', kind: 'backlight', handle: 'intel_backlight', percent: 60 }],
//   ddc: { state: 'detecting', reason: null, unsupported: [] } }

const unlisten = await onBrightnessChanged((next) => { /* … */ });

// `handle` sale del informe: el bus i2c en un monitor externo.
await setBrightness('ddc', '5', 70);

const night = await getNightLight();
await setNightLight({ ...night.config, nightTemperature: 3500 });
```

| Comando | Devuelve |
|---|---|
| `get_brightness` | `BrightnessReport` |
| `set_brightness(kind, handle, percent)` | — (`handle` es el de `MonitorBrightness`: el nombre de la retroiluminación, como `intel_backlight`, o el número de bus i2c del monitor externo, como `"5"` para `/dev/i2c-5`) |
| `refresh_brightness` | — (el resultado llega por el evento) |
| `get_night_light` | `NightLight` |
| `set_night_light(config)` | `NightLight` |

## Dependencias en tiempo de ejecución

- `systemd` (logind) para escribir la retroiluminación.
- `ddcutil`, **opcional**, para los monitores externos, con el módulo
  `i2c-dev` cargado y acceso a `/dev/i2c-*`.
- `wlsunset`, **opcional**, para la luz nocturna.

## Pruebas

```sh
cargo test                                         # sin hardware
cargo test --release -- --ignored --nocapture      # contra esta máquina: mide y escribe en el panel el brillo que ya tiene
bun run test                                       # el binding de JavaScript
```

## Licencia

GPL-3.0-or-later.
