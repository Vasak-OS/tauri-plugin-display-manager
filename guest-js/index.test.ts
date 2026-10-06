import { beforeEach, describe, expect, it, mock } from 'bun:test';

const invokeMock = mock((_: string, __?: Record<string, unknown>) => Promise.resolve(undefined));
const listenMock = mock((_: string, __: (event: { payload: unknown }) => void) =>
	Promise.resolve(() => {}),
);

// Un solo `mock.module` por módulo: dos del mismo dan verde local y rojo en CI.
mock.module('@tauri-apps/api/core', () => ({ invoke: invokeMock }));
mock.module('@tauri-apps/api/event', () => ({ listen: listenMock }));

const report = {
	monitors: [{ output: 'eDP-1', kind: 'backlight', handle: 'intel_backlight', percent: 40 }],
	ddc: { state: 'detecting', reason: null, unsupported: [] },
};

const config = {
	mode: 'manual',
	dayTemperature: 6500,
	nightTemperature: 4000,
	sunrise: '07:00',
	sunset: '20:00',
	latitude: null,
	longitude: null,
} as const;

describe('display-manager desde el frontend', () => {
	beforeEach(() => {
		invokeMock.mockClear();
		listenMock.mockClear();
	});

	it('pide el brillo sin argumentos', async () => {
		invokeMock.mockResolvedValueOnce(report as never);
		const mod = await import('./index');

		expect(await mod.getBrightness()).toEqual(report as never);
		expect(invokeMock).toHaveBeenCalledWith('plugin:display-manager|get_brightness');
	});

	it('cambia el brillo con los nombres de argumento que espera Rust', async () => {
		const mod = await import('./index');

		await mod.setBrightness('ddc', '5', 70);

		expect(invokeMock).toHaveBeenCalledWith('plugin:display-manager|set_brightness', {
			kind: 'ddc',
			handle: '5',
			percent: 70,
		});
	});

	it('pide releer los monitores externos', async () => {
		const mod = await import('./index');
		await mod.refreshBrightness();
		expect(invokeMock).toHaveBeenCalledWith('plugin:display-manager|refresh_brightness');
	});

	it('escucha el evento del plugin y entrega sólo el informe', async () => {
		const mod = await import('./index');
		const seen: unknown[] = [];

		await mod.onBrightnessChanged((r) => seen.push(r));

		const [name, callback] = listenMock.mock.calls[0];
		expect(name).toBe('display-brightness-changed');
		expect(name).toBe(mod.BRIGHTNESS_EVENT);
		callback({ payload: report });
		expect(seen).toEqual([report]);
	});

	it('lee y guarda la luz nocturna envolviendo la configuración', async () => {
		const mod = await import('./index');

		await mod.getNightLight();
		expect(invokeMock).toHaveBeenCalledWith('plugin:display-manager|get_night_light');

		await mod.setNightLight(config);
		expect(invokeMock).toHaveBeenCalledWith('plugin:display-manager|set_night_light', {
			config,
		});
	});
});
