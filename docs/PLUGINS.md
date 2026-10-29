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
depends: [yam/luna ^0.2]
files:
  share/luna/plugins/vasya.clock/: plugin/
```

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

## Пользовательская веб-страница

Плагин может зарегистрировать собственную HTML-страницу внутри Luna. Для этого
добавьте разрешение `web`, положите HTML в каталог пакета и вызовите функцию из
`main.lua`:

```toml
permissions = ["web"]
launch = { mode = "service" }
```

```lua
local url = register.customweb("web/index.html")
print("web page: " .. url)

while true do
  alice.sleep(60000)
end
```

Страница будет доступна по адресу `/web/<plugin-id>`, например
`/web/vasya.clock`, и появится отдельной вкладкой в веб-панели Luna. Путь к HTML
ограничен каталогом самого пакета, принимаются только `.html` и `.htm` размером
до 4 МБ. Страница живёт, пока запущен плагин; при его остановке регистрация
удаляется. Это позволяет странице использовать обычные запросы к Luna API
через `fetch('/api/...')`.

Сборка и установка выполняются обычными командами Yam:

```sh
yam-publish pack .
yam install vasya/clock
```

Luna ищет каталог через `--yam-root`, `YAM_ROOT`, либо автоматически по пути
установленного бинаря вида `$YAM_ROOT/pkgs/yam/luna/<version>/bin/luna`.
Для блокировки чтения во время транзакций Yam можно передать `--yam-run` или
задать `YAM_RUN`; Luna использует `$YAM_RUN/yam.lock` как shared lock. Если
временный каталог или lock-файл отсутствуют после перезагрузки, Luna создаёт
их без замены существующего файла. Если
`YAM_ROOT` известен, а `YAM_RUN` нет, Luna продолжает работу без блокировки и
пишет одно предупреждение.

Плагины из Yam доступны в `GET /api/plugins` с полем `source: "yam"`. Их
нельзя удалить через Luna или заменить загрузкой `.yap`; используйте:

```sh
yam remove vasya/clock
```

Состояние Luna и runtime log не записываются в дерево Yam. Они находятся в
`<data-dir>/plugins/plugin-state/<id>/`. Бинарные плагины запускаются при
`allow_plugin_binaries=true` и разрешении `exec`, как локальные пакеты.
YAM владеет файлами, Luna владеет процессом и runtime-токеном. Не объявляйте
второй сервис YAM для того же процесса. Перед обновлением остановите плагин.
Процесс получает `YAM_ROOT`, `YAM_VAR` и `LUNA_PLUGIN_STATE`; пользовательские
данные записывайте в var/state, каталог установленного плагина доступен только
для чтения. Для автозапуска включите сервис через API или панель Luna.

Изменения обнаруживаются при старте, при ручном `POST /api/plugins/rescan` и
фоновым сканированием раз в 30 секунд. Источники можно посмотреть через
`GET /api/plugins/sources`.

### Автозапуск на BusyBox init

В интеграционной сборке есть `tools/luna-service`: он запускает Luna на
`127.0.0.1:8099`, хранит состояние в `$YAM_ROOT/var/yam.luna` и создаёт временные
каталоги после reboot. `luna-service install` устанавливает управляемый hook
`/etc/init.d/S99zluna`; затем выполните `luna-service start`. Это вариант для
установок YAM без супервизора сервисов. Не включайте одновременно второй
способ автозапуска Luna. `luna-service uninstall` останавливает процесс и
удаляет только управляемый hook.

Пакет для одной станции после сборки её бинаря создаётся командой
`tools/package-station.sh MODEL OUTPUT.yampkg`. Пути к host-пакеру YAM и профилям
задаются через `YAM_BIN` и `YAM_PROFILES`. Проверка запуска с пустым временным
каталогом: `python3 tests/yam_service.py target/debug/luna`.
