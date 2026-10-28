# Lua-плагины через YAM

Luna может читать Lua-плагины, установленные пакетным менеджером YAM. Yam
владеет файлами, а Luna только читает их и запускает Lua с теми же правами,
которые описаны в `plugin.toml`.

Плагин размещается в общем дереве:

```text
share/luna/plugins/<namespace>.<name>/
├── plugin.toml
└── main.lua
```

Каталог должен совпадать с id пакета YAM: например, `vasya/clock` становится
`vasya.clock`. В `plugin.toml` id должен быть таким же.

## Пример пакета

`yam.yaml`:

```yaml
name: vasya/clock
version: 1.0.0
license: MIT
description: Часы на кольце
targets: [any]
files:
  share/luna/plugins/vasya.clock/: plugin/
```

Текущая версия Yam не поддерживает поле `depends`, поэтому Luna устанавливается
отдельно до установки плагина.

`plugin/plugin.toml`:

```toml
id = "vasya.clock"
name = "Clock"
version = "1.0.0"
entry = "main.lua"
permissions = ["led"]

[compatibility]
features = ["led.ring.rgb"]
```

`plugin/main.lua`:

```lua
if not device.has_ring then
  error("ring is unavailable")
end

alice.led({colors = {"#00AAFF"}})
```

Сборка и установка выполняются обычными командами Yam:

```sh
yam-publish pack .
yam install vasya/clock
```

Luna ищет каталог через `--yam-root`, `YAM_ROOT`, либо автоматически по пути
установленного бинаря вида `$YAM_ROOT/pkgs/yam/luna/<version>/bin/luna`.
Для блокировки чтения во время транзакций Yam можно передать `--yam-run` или
задать `YAM_RUN`; Luna использует `$YAM_RUN/yam.lock` как shared lock. Если
`YAM_ROOT` известен, а `YAM_RUN` нет, Luna продолжает работу без блокировки и
пишет одно предупреждение.

Плагины из Yam доступны в `GET /api/plugins` с полем `source: "yam"`. Их
нельзя удалить через Luna или заменить загрузкой `.yap`; используйте:

```sh
yam remove vasya/clock
```

Состояние Luna и runtime log не записываются в дерево Yam. Они находятся в
`<data-dir>/plugins/plugin-state/<id>/`. Lua-плагины поддерживаются, binary-
плагины из Yam пока намеренно отклоняются.

Изменения обнаруживаются при старте, при ручном `POST /api/plugins/rescan` и
фоновым сканированием раз в 30 секунд. Источники можно посмотреть через
`GET /api/plugins/sources`.
