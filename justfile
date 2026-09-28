default: build

build:
	cargo build --workspace --release

export NAME := 'cosmic-ext-applet-calendar'
export APPID := 'io.github.hasmolam.cosmic-ext-applet-calendar'

cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
bin-src := cargo-target-dir / 'release' / NAME
time-bin-src := cargo-target-dir / 'release' / 'cosmic-applet-time'
daemon-bin-src := cargo-target-dir / 'release' / 'cosmic-calendar-sync'

rootdir := ''
prefix := '/usr'

base-dir := absolute_path(clean(rootdir / prefix))
share-dst := base-dir / 'share'

bin-dst := base-dir / 'bin' / NAME
desktop-dst := share-dst / 'applications' / APPID + '.desktop'
icon-dst := share-dst / 'icons/hicolor/scalable/apps' / APPID + '-symbolic.svg'

install: build
	install -Dm0755 {{ bin-src }} {{ bin-dst }}
	install -Dm0755 {{ daemon-bin-src }} {{ base-dir / 'bin' / 'cosmic-calendar-sync' }}
	install -Dm0644 data/icons/scalable/apps/{{ APPID }}-symbolic.svg {{ icon-dst }}
	install -Dm0644 data/{{ APPID }}.desktop {{ desktop-dst }}
	install -Dm0644 data/dev.cosmic.Calendar.desktop {{ share-dst / 'applications/dev.cosmic.Calendar.desktop' }}
	install -Dm0644 data/systemd/user/dev.cosmic.Calendar.service {{ share-dst / 'systemd/user/dev.cosmic.Calendar.service' }}
	install -Dm0644 data/dbus-1/services/dev.cosmic.Calendar.service {{ share-dst / 'dbus-1/services/dev.cosmic.Calendar.service' }}

install-user:
	just build
	install -Dm0755 {{ bin-src }} {{ env_var('HOME') }}/.local/bin/{{ NAME }}
	install -Dm0755 {{ daemon-bin-src }} {{ env_var('HOME') }}/.local/bin/cosmic-calendar-sync
	install -Dm0644 data/dev.cosmic.Calendar.desktop {{ env_var('HOME') }}/.local/share/applications/dev.cosmic.Calendar.desktop
	install -Dm0644 data/systemd/user/dev.cosmic.Calendar.service {{ env_var('HOME') }}/.config/systemd/user/dev.cosmic.Calendar.service
	install -Dm0644 data/dbus-1/services/dev.cosmic.Calendar.service {{ env_var('HOME') }}/.local/share/dbus-1/services/dev.cosmic.Calendar.service
	sed -i 's|^Exec=.*|Exec={{ env_var("HOME") }}/.local/bin/cosmic-calendar-sync|' {{ env_var('HOME') }}/.local/share/dbus-1/services/dev.cosmic.Calendar.service
	systemctl --user daemon-reload || true

uninstall:
	rm -f {{ bin-dst }}
	rm -f {{ base-dir / 'bin' / 'cosmic-calendar-sync' }}
	rm -f {{ icon-dst }}
	rm -f {{ desktop-dst }}
	rm -f {{ share-dst / 'applications/dev.cosmic.Calendar.desktop' }}

uninstall-user:
	rm -f {{ env_var('HOME') }}/.local/bin/{{ NAME }}
	rm -f {{ env_var('HOME') }}/.local/bin/cosmic-calendar-sync
	rm -f {{ env_var('HOME') }}/.local/share/applications/dev.cosmic.Calendar.desktop
	rm -f {{ env_var('HOME') }}/.config/systemd/user/dev.cosmic.Calendar.service
	rm -f {{ env_var('HOME') }}/.local/share/dbus-1/services/dev.cosmic.Calendar.service
	systemctl --user daemon-reload || true
