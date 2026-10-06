import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

/**
 * Por dónde se llega al brillo de una pantalla: la retroiluminación de un panel
 * interno, o DDC/CI en un monitor externo.
 */
export type BrightnessKind = 'backlight' | 'ddc';

export interface MonitorBrightness {
	/** El conector DRM (`eDP-1`, `DP-2`), para cruzarlo con la lista de monitores. */
	output: string | null;
	kind: BrightnessKind;
	/** Lo que hay que devolver en `setBrightness`. */
	handle: string;
	percent: number;
}

/**
 * - `ready`: lo que hay en `monitors` es todo lo que se sabe.
 * - `detecting`: se están buscando los monitores externos; llegan por el evento.
 * - `unavailable`: hay monitores externos pero no hay cómo hablarles (`reason`).
 */
export type DdcState = 'ready' | 'detecting' | 'unavailable';

/** Códigos, no frases: el texto lo pone cada aplicación, traducido. */
export type DdcUnavailableReason = 'not-installed' | 'no-i2c-dev' | 'no-permission';

export interface DdcStatus {
	state: DdcState;
	reason: DdcUnavailableReason | null;
	/** Conectores de monitores externos que no contestan por DDC/CI. */
	unsupported: string[];
}

export interface BrightnessReport {
	monitors: MonitorBrightness[];
	ddc: DdcStatus;
}

export type NightLightMode = 'manual' | 'location';

export interface NightLightConfig {
	mode: NightLightMode;
	/** Kelvin de día. */
	dayTemperature: number;
	/** Kelvin de noche; menor que la de día. */
	nightTemperature: number;
	/** `HH:MM` en que empieza el día (modo manual). */
	sunrise: string;
	/** `HH:MM` en que empieza la noche (modo manual). */
	sunset: string;
	latitude: number | null;
	longitude: number | null;
}

export interface NightLight {
	/** Falso sin `wlsunset` instalado. */
	available: boolean;
	/** Si ya se guardó alguna vez; si no, `config` son los valores por omisión. */
	configured: boolean;
	config: NightLightConfig;
}

/** El evento que se emite cuando cambia el brillo o cambian los monitores. */
export const BRIGHTNESS_EVENT = 'display-brightness-changed';

/**
 * El brillo de cada pantalla. No espera a DDC/CI: los monitores externos que
 * todavía no se conocen llegan después por `onBrightnessChanged`.
 */
export async function getBrightness(): Promise<BrightnessReport> {
	return await invoke<BrightnessReport>('plugin:display-manager|get_brightness');
}

export async function setBrightness(
	kind: BrightnessKind,
	handle: string,
	percent: number,
): Promise<void> {
	await invoke('plugin:display-manager|set_brightness', { kind, handle, percent });
}

/** Vuelve a leer los monitores externos; el resultado llega por el evento. */
export async function refreshBrightness(): Promise<void> {
	await invoke('plugin:display-manager|refresh_brightness');
}

/**
 * Avisa cuando cambia el brillo de cualquier pantalla (también desde una tecla
 * u otro programa) o cuando cambian los monitores conectados.
 */
export async function onBrightnessChanged(
	handler: (report: BrightnessReport) => void,
): Promise<UnlistenFn> {
	return await listen<BrightnessReport>(BRIGHTNESS_EVENT, (event) => handler(event.payload));
}

export async function getNightLight(): Promise<NightLight> {
	return await invoke<NightLight>('plugin:display-manager|get_night_light');
}

/** Valida y guarda la configuración. No enciende ni apaga la luz nocturna. */
export async function setNightLight(config: NightLightConfig): Promise<NightLight> {
	return await invoke<NightLight>('plugin:display-manager|set_night_light', { config });
}
