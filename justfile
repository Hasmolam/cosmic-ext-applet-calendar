default: build

build:
	cargo build --release

export NAME := 'cosmic-ext-applet-calendar'
export APPID := 'io.github.hasmolam.cosmic-ext-applet-calendar'

cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
bin-src := cargo-target-dir / 'release' / NAME
time-bin-src := cargo-target-dir / 'release' / 'cosmic-applet-time'

rootdir := ''
prefix := '/usr'

base-dir := absolute_path(clean(rootdir / prefix))
share-dst := base-dir / 'share'

bin-dst := base-dir / 'bin' / NAME
desktop-dst := share-dst / 'applications' / APPID + '.desktop'
icon-dst := share-dst / 'icons/hicolor/scalable/apps' / APPID + '-symbolic.svg'

install:
	install -Dm0755 {{ bin-src }} {{ bin-dst }}
	install -Dm0644 data/icons/scalable/apps/{{ APPID }}-symbolic.svg {{ icon-dst }}
	install -Dm0644 data/{{ APPID }}.desktop {{ desktop-dst }}

uninstall:
	rm -f {{ bin-dst }}
	rm -f {{ icon-dst }}
	rm -f {{ desktop-dst }}
