# Luna

Luna — отдельный runtime для станций Яндекса. Он предоставляет HTTP API,
Lua runtime, веб-интерфейс и общий слой capabilities, а аппаратная реализация
каждой станции находится в отдельном crate.

Полная документация API: [docs/API.md](docs/API.md).

## Как это устроено

`src/main.rs` — минимальная точка запуска. Шлюз, HTTP API, Lua sandbox,
permissions и package manager находятся в `src/gateway.rs`.

Каждая поддерживаемая станция имеет собственную папку:

```text
stations/<device>/
├── Cargo.toml
├── station_main.rs
└── hardware.rs / device.rs / ...
```

Station crate определяет hardware backend и доступные capabilities. Общий
gateway не содержит таблиц с аппаратными параметрами конкретных моделей.
Список бинарников, моделей, targets и release assets находится в
[`luna-manifest.json`](luna-manifest.json).

## Поддерживаемые станции

| Модель | Бинарник | Target |
| --- | --- | --- |
| `yandexmini3` | `luna-yandexmini3` | `aarch64-unknown-linux-musl` |
| `yandexmini` | `luna-yandexmini` | `armv7-unknown-linux-musleabihf` |
| `yandexmicro` | `luna-yandexmicro` | `aarch64-unknown-linux-musl` |
| `yandexmini_2` | `luna-yandexmini_2` | `aarch64-unknown-linux-musl` |
| `yandexmidi` | `luna-yandexmidi` | `aarch64-unknown-linux-musl` |
| `yandexplum` | `luna-yandexplum` | `aarch64-unknown-linux-musl` |

Если модель отсутствует в manifest, она считается неподдерживаемой. Для новой
станции нужно добавить её crate, capabilities и запись в manifest.

## Сборка

Обычная сборка host-бинарника:

```sh
cargo build --release --locked
```

Сборка всех station-бинарников и создание `dist/`:

```sh
tools/build-stations.sh
```

Сборка одной станции:

```sh
tools/build-stations.sh luna-manifest.json dist yandexmidi
```

Или интерактивный выбор станции:

```sh
tools/build.sh
```

На macOS нужны Rust targets из manifest и соответствующие musl cross-linker-ы:

```sh
rustup target add aarch64-unknown-linux-musl
rustup target add armv7-unknown-linux-musleabihf
```

Скрипт сборки автоматически использует `aarch64-linux-musl-gcc` и
`arm-linux-musleabihf-gcc`, если они доступны в `PATH`.

`dist/manifest.json` содержит список собранных assets и SHA-256, а
`dist/SHA256SUMS` — checksum-файл для проверки загрузки.

## API и capabilities

Основные группы API:

- `/api/device` — модель, архитектура и capabilities станции;
- `/api/led`, `/api/led/ring`, `/api/led/logo` — управление LED;
- `/api/clock` — сегментные часы;
- `/api/input` — устройства ввода, кнопки и распознанные свайпы;
- `/api/sensors` — доступные датчики;
- `/api/plugins` и `/api/runtime` — плагины и Lua runtime.

Успешная синхронная операция возвращает `200 OK`. Запуск асинхронной операции
возвращает `202 Accepted`. Ошибка возвращается с соответствующим HTTP-кодом и
JSON вида:

```json
{"error":"описание ошибки"}
```

Поддержку capability можно проверить через `/api/device` или в Lua через
`.is_available()`. Для LED также доступны свойства устройства:
`device.has_ring`, `device.ring_count`, `device.has_logo` и `device.logo_count`.

Примеры всех endpoint-ов находятся в [docs/API.md](docs/API.md).

## Разработка станции

Добавьте новый crate в `stations/<device>/`, реализуйте `station_main.rs` и
нужные hardware/capability модули, затем добавьте его в workspace и
`luna-manifest.json`. После этого новая модель будет собираться отдельным
бинарником и публиковаться как отдельный release asset.

Перед отправкой изменений проверьте workspace:

```sh
cargo test --workspace --locked
```

`tools/build-deploy.sh` и `tools/release.sh` используются локально владельцем
репозитория для deploy и публикации релизов и намеренно не входят в Git.
Локальный `tools/release.sh` одним запуском собирает все станции, создаёт
station-specific `.yampkg`, публикует их в YAM Registry и добавляет бинарники,
пакеты и checksum-файлы в GitHub Release:

```sh
tools/release.sh --update
```

Для registry скрипт использует `publish.env` и ключ из каталога публикации
YAM, который можно переопределить через `--yam-publish-dir`.

## Лицензия

Проект находится в разработке. Условия распространения будут добавлены
отдельно.
