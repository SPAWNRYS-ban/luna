# Luna

Luna — отдельный host-пакет для Lua, Quasar API, веб-интерфейса, старых `.yap`,
`plugin.toml` и их модели разрешений. Ядро `yam` не читает, не устанавливает и
не запускает такие плагины.

Полная документация HTTP API: [docs/API.md](docs/API.md).

Сборка:

```sh
cargo build --release --locked
```

Затем укажите собранный бинарный файл для нужной платформы в `yam.yaml` и
соберите пакет командой `yam pack` из репозитория YAM.

`src/main.rs` — только шлюз запуска. HTTP API, Lua sandbox, permissions и
package manager находятся в `src/gateway.rs`. Каждая поддерживаемая колонка —
отдельный crate в `stations/<model>/` со своим `station_main.rs` и capability
contract. Так добавляются аппаратные реализации, не разрастается gateway и
можно проверять колонку отдельной сборкой.

Для станций runtime можно собирать отдельными бинарями с профилем модели. Они
используют общий gateway, но capability contract и имя бинаря принадлежат
конкретной колонке:

```sh
tools/build-stations.sh                 # все модели из luna-manifest.json
tools/build-stations.sh luna-manifest.json dist yandexmicro
tools/build.sh                           # интерактивно собрать одну станцию

# собрать все station-бинарники и опубликовать GitHub Release
tools/release.sh --tag v0.2.1
```

Собрать и сразу залить выбранную станцию через ADB:

```sh
tools/build-deploy.sh --device yandexmidi --serial <adb-serial>
```

`tools/build-deploy.sh` собирает и разворачивает только выбранную станцию.
Если `--device` или `--serial` не указаны, скрипт предлагает выбрать их
стрелками. Для публикации полного релиза используется отдельный
`tools/release.sh`; ему нужен авторизованный GitHub CLI (`gh auth login`).

Скрипт проверяет модель станции, собирает только её target, загружает бинарь,
делает backup предыдущего, перезапускает Luna и проверяет `/api/device` через
ADB port-forward. Путь и порт можно изменить через `--root` и `--listen`.

`dist/manifest.json` содержит target, asset, модели и SHA-256 каждого бинаря.
`luna` и `luna-universal` — универсальные бинарники без station-specific
hardware backend-ов. Они объединяют общие runtime capabilities и подходят для
проверки API. Для физического управления конкретной колонкой используются
station-бинарники: `luna-yandexmini`, `luna-yandexmicro`,
`luna-yandexmini_2`, `luna-yandexmini3`, `luna-yandexmidi` и `luna-yandexplum` запускаются с
профилем, зашитым в имя бинаря.

Если модель отсутствует в `luna-manifest.json`, она считается неподдерживаемой
и бинарь для неё не собирается. Для добавления поддержки создайте новый
`stations/<model>/Cargo.toml`, `station_main.rs`, capability contract и запись
в manifest; после этого сборка и установщик смогут выбрать новый asset.

`tools/convert_apkg.py` — офлайн-конвертер APKG в `.yap` только для этого
runtime. Он не создаёт и не устанавливает `.yampkg`.
