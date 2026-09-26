# Архитектура

## Как всё работает

```mermaid
flowchart TD
    A[старт процесса] --> B[сборка неизменяемого реестра апплетов]
    B --> C{скрытый re-exec встроенной команды?}
    C -- да --> D[запуск зарегистрированного апплета]
    C -- нет --> E{argv0 совпадает с именем апплета?}
    E -- да --> D
    E -- нет --> F{управляющая опция axe?}
    F -- --applet --> D
    F -- help/version/list --> H[ответ axe]
    F -- обычные аргументы shell --> I[Brush shell]
    I --> J[alias или функция]
    J --> K[встроенная команда shell]
    K --> L[встроенный апплет]
    L --> M[апплет по запросу]
    M --> N[исполняемый файл из PATH]
```

`crates/axe/src/main.rs` разбирает верхнеуровневые `help` и `version` до сборки реестра. Для обычного dispatch неизменяемый реестр строится один раз. Это важно: встроенные команды могут рекурсивно запускать другие встроенные команды — например, так делает `xargs`.

Основной бинарник называется `axe`; `brush-shell` является внутренней зависимостью.

Vendored-набор Brush (`brush-shell`, `brush-core`, `brush-builtins`,
`brush-parser`, `brush-interactive` и `brush-coreutils-builtins`) зафиксирован
на immutable upstream revision `08db87a62345f11a4fc43bf385e318677f641089`;
версии и пути всех шести crates записаны вместе в
`workspace.metadata.brush-fork`. Поверх этого набора сохранено интеграционное
расширение AXE. Через `install_with_executable` команда `axe` передаёт шимам
provider, который непосредственно перед каждым bundled launch заново выбирает
`BundledExecutable`: обычный path либо Linux descriptor с одним optional
checked path fallback. Explicit `None` запрещает lazy-вызов `current_exe()`.
Поэтому shell создаётся даже без re-exec capability, а shim возвращает status
126 только при фактической попытке запустить bundled child.

## Source workspace и edition bundle

Rust workspace и общий Nix package API живут в public repository. Distribution
задаётся отдельным edition bundle: `edition.json`, три config-файла, SSH/relay
и Store trust material, generated bootstrap, release metadata, optional
additional CA и private package categories. `AXE_EDITION_ROOT` выбирает bundle
целиком; смешивать отдельные inputs из разных roots нельзя. Без переменной
source workspace одновременно является OSS edition root.

`crates/axe/build.rs` проверяет edition ID и все selected inputs до генерации
констант. `axe-tls-roots/build.rs` использует тот же root: public build получает
только Mozilla WebPKI roots, а external edition может добавить PEM bundle.
Runtime публикует edition ID через `--version` и `doctor`.

Public flake экспортирует единственный конструктор
`lib.axeStore.mkPackageSet`. Он принимает optional `additionalCaBundle` и
функцию `extraCategories`; external flake добавляет private package attributes,
не копируя public modules и не вводя второй metadata schema.

## Реестр апплетов

`crates/axe/src/registry.rs` — единственное место, где собирается runtime-реестр:

1. импортирует включённые команды из uutils/coreutils;
2. регистрирует собственные P0-адаптеры под features Cargo;
3. регистрирует демон-апплеты;
4. добавляет проверенный AXE Store Index, не затирая имена встроенных команд;
5. разбирает и применяет алиасы, встроенные из `config/aliases.json`.

Алиас отклоняется, если его имя уже занято командой или `argv` пуст. Первый элемент `argv` выбирает скомпилированный апплет, остальные добавляются перед пользовательскими аргументами. Необязательный массив `help` переопределяет поведение `-h` и `--help`. Если feature целевого апплета выключен, алиас просто не попадает в реестр — сломанной команды там не остаётся.

При прямом запуске апплета `argv[0]` собирается из имени выбранного апплета. Для алиаса используется полный настроенный `argv`. Аргументы остаются в виде `OsString`; преобразование в UTF-8 происходит только для имён команд и upstream API, которым нужны строки.

Каждая запись реестра создаётся вместе с metadata: canonical name, source
`bundled` или `store`, category, optional alias и synopsis. Dispatch-таблица и
отсортированный inventory получаются из одного builder-а, поэтому параллельного
реестра имён нет. Для category AXE Store берётся из подписанного `id`, synopsis и
aliases — из Index.

Всегда встроенный applet `commands` публикует отсортированный snapshot metadata
реестра как versioned JSON `axe_commands` v1. Основной интерфейс рассчитан на
вызов непосредственно из Brush: `commands | jq ...`; `axe` перед applet не
нужен. `commands NAME` возвращает metadata одной команды, не запуская её, и
status 127 для неизвестного имени. Когда `AXE_STORE_MODE=off`, ошибка lookup
дополнена `store_mode: "off"` и `store_registry: "disabled"`: это отличает
отключённую Store-поверхность от доказанного отсутствия имени в AXE Store.
Каждая запись дополнена runtime-полями
`availability`: `local`, `on_demand` или `blocked`, и nullable `local_path`.
Последний указывает только на существующий applet в опубликованном PATH bridge;
не-UTF-8 path кодируется тем же `{ "base64": "..." }`, что в `axe_doctor`.
Динамические aliases и functions текущего процесса shell в snapshot не входят —
для их разрешения используются стандартные `type` и `command -v`.

`commands` является пассивным inventory API. Отдельный `doctor` строит
versioned schema `axe_doctor` v2 из общего typed report. Human renderer по
умолчанию показывает краткие evidence-based `Summary` и `Attention`, затем
свёрнутые наблюдения. Он скрывает повторный `Active checks`, absent environment
variables и пустую `Degradations`; `--verbose` раскрывает их вместе с launch
candidates и отдельными probe stages. `--json` сериализует полный typed report
без human-only summary или оценки общего «здоровья».
Isolation layers are serialized with `interpretation: "heuristic_indicator"`;
provider signatures and confidence never become host-wide absence verdicts.
Активные probes создают и удаляют private test objects в `--path`,
`$AXE_WORK_DIR` или current directory. Environment observation покрывает
фиксированный набор runtime-переменных (`HOME`, `PATH`, `SHELL`, `AXE_*`,
locale/terminal/runtime directory); default human output скрывает absent
variables, а verbose и JSON сохраняют явный статус. Остальные переменные вообще
не включаются. UTF-8 paths и значения сериализуются обычными JSON strings,
non-UTF-8 bytes — объектом `{ "base64": "..." }`.

## PATH и self-exec capability

`crates/axe/src/executable.rs` — единственный владелец self-exec capability. Он инициализируется сразу после чтения argv и хранится до завершения процесса. На Linux сначала принимается внутренний inherited descriptor, затем открывается `/proc/self/exe`, затем readable filesystem candidate. Inherited fd валидируется до принятия и сразу получает `FD_CLOEXEC`; внутренние переменные окружения удаляются до запуска потоков.

Тот же owner выбирает runtime root лениво и ровно один раз. Приоритет имеют
явный `AXE_WORK_DIR`, проверенный `__AXE_RUNTIME_ROOT` от descriptor-backed
родителя, `$XDG_CACHE_HOME/axe`, Linux `$HOME/.cache/axe` или Darwin
`$HOME/Library/Caches/axe`, `$XDG_RUNTIME_DIR/axe`, затем
`$TMPDIR/axe-<uid>`. Относительные XDG paths игнорируются. Автоматический root
принадлежит effective UID, имеет mode `0700`
и не может быть symlink. Runtime policy не перебирает mounts из
`/proc/self/mountinfo`; появление нового mount не меняет выбор. Descriptor
launch передаёт canonical root следующему AXE process, который удаляет
служебную переменную до запуска потоков. Явный `AXE_WORK_DIR` имеет приоритет
над handoff и при ошибке не допускает fallback. Storage roots AXE Store
выбираются отдельной policy и не влияют на runtime root.

Если procfs недоступен и AXE запущен только по pathname, между kernel `exec` и
ранним `initialize()` остаётся фундаментальное окно: удалённый до открытия inode
восстановить нельзя. Эту гарантию даёт только inherited executable fd от
launcher-а. Retained fd readable; отдельный exec-only `O_PATH` capability не
моделируется.

Proc descriptor принимается, только пока `/proc/self/fd/<fd>` резолвится в сохранённые device/inode. Обычная публикация bridge использует тот же выбранный original-or-relay path, что `SHELL` и `AXE_SHELL`; live `/proc/<pid>/exe` используется как последний bridge fallback, когда conventional path недоступен. `/proc/self/fd/<fd>` наружу не публикуется, потому что fd принадлежит процессу-публикатору. Filesystem candidate берётся из `current_exe()`, затем на Linux из байтов `AT_EXECFN`, разрешённых относительно startup current directory. Path canonicalize’ится и принимается только для regular executable file; при доступном retained descriptor inode обязан совпасть.

Descriptor launch дублирует fd с `F_DUPFD_CLOEXEC`, предпочитая диапазон от 256 и переходя к low fd при малом `RLIMIT_NOFILE`; старые ядра получают `F_DUPFD` плюс `FD_CLOEXEC`. Полные `argv` и `envp` строятся в parent. Последний `pre_exec` после redirections, cwd, session/process-group setup снимает `CLOEXEC`, затем вызывает `execveat(fd, "", ..., AT_EMPTY_PATH)`, `fexecve` и только после их отказа checked path fallback. Внутренний fd и fallback передаются child через environment, поэтому новый AXE process снова владеет тем же capability. `argv[0]`, byte-oriented аргументы, environment и caller-registered `pre_exec` semantics сохраняются.

Brush выбирает Tokio runtime после выбора input backend: Basic и Reedline
работают на multi-thread runtime, потому что их event loops используют blocking
API; non-interactive/minimal input использует current-thread runtime. Ошибка
spawn относится к конкретной команде.
Bundled applets требуют child AXE, а `doctor` зарегистрирован как native builtin
callback: он читает live shell cwd, flags и exported environment и доступен при
запрете новых процессов.

Linux lazily готовит portable filesystem relay для path-only consumers и единственного checked path fallback descriptor launch. Исходный path выбирается, только если mount policy подтверждает execution. При `noexec` или потерянном original path выбирается relay; при `unknown` или неподдерживаемом определении policy AXE сначала пытается создать relay и только затем оставляет original path. Из retained fd image копируется через `read_at` во временный `create_new` regular file внутри user-owned `.axe-self` с mode `0700`; после `fsync`, повторной проверки source metadata и `chmod 0500` файл публикуется hard link как `.axe-self/<40 hex SHA-1 ELF build ID>`. При доступном `flock` принятый relay удерживает shared lease. Под exclusive directory lock GC оставляет current и один предыдущий relay; leased файлы удаляются после завершения использующего их процесса. Если filesystem или sandbox запрещает `flock`, AXE использует `nolock-<40 hex>`, который GC не трогает; публикация работает без автоматической очистки.

Relay сохраняет executable bytes, но не filesystem security object: новый inode
намеренно имеет mode `0500` и не наследует file capabilities, LSM labels,
security xattrs или setuid/setgid semantics. Provider вызывается перед каждым
bundled launch, поэтому после потери original path может materialize relay в
parent до `fork`.

Порядок backend-ов для bundled child: retained descriptor через `execveat`, тот же descriptor через `fexecve`, затем один заново выбранный inode-checked path — original или relay. Они не образуют две последовательные попытки `execve`. `/proc/self/fd/<fd>` не передаётся Brush как обычный path: shell закрывает unrelated descriptors до `exec`, и такой path становится dangling. Self-exec не использует `memfd_create`, executable `mmap` или `mprotect(PROT_EXEC)`: MDWE не мешает backend-у, а seccomp/старое ядро с запрещённым descriptor exec деградирует к обычному `execve` выбранного path.

| Capability | `SHELL` | `AXE_SHELL` | Bundled child / bridge |
|---|---|---|---|
| Linux retained descriptor + executable original path | canonical path | canonical path | retained fd, original fallback / canonical path |
| Linux retained descriptor + `noexec` or missing original + relay | relay path | relay path | retained fd, relay fallback / relay |
| Linux retained descriptor без relay/path | inherited value не меняется | удаляется | descriptor-aware launch / bridge отсутствует |
| Darwin filesystem | canonical path | canonical path | canonical path |
| Нет descriptor или проверенного path | inherited value не меняется | удаляется | re-exec и bridge отсутствуют |

Последняя строка не блокирует создание Brush: direct, positional, BusyBox, hidden и `--applet` dispatch выполняются in-process. Descriptor syscall denial без executable relay/path даёт контролируемую cannot-execute/NotFound ошибку только когда требуется новый process. Path-only nested implementation после unlink требует relay; descriptor-aware Brush, supervisor и SSH launchers используют retained fd напрямую.

Каждый AXE-managed process экспортирует стабильный маркер `AXE=true` до
инициализации executable state. Brush shell, SSH shell/exec и их дочерние
процессы наследуют его независимо от доступности PATH bridge. Маркер сообщает
только принадлежность environment к AXE; версия и runtime evidence остаются в
`doctor`.

Перед входом в Brush `crates/axe/src/path_bridge.rs` вычисляет SHA-256 отсортированных имён runtime-реестра, усечённый до 16 hex-символов. В единственном выбранном runtime root создаётся `.axe-<pin>`. Полный набор BusyBox-style symlink’ов публикуется новой immutable generation, после чего symlink `current` переключается atomic rename.

Существующий `current` переиспользуется после проверки точного набора имён и каждого symlink target. Все entry поколения имеют единый target, сверяемый по device/inode. Missing, лишняя или повреждённая entry приводит к новой generation. Lock сериализует startup.

Inherited `AXE_APPLET_DIR` сначала проверяется как bridge текущего executable, затем его точные вхождения удаляются из `PATH`. После успешной публикации новый `current/bin` стоит первым и встречается ровно один раз. При отсутствии publishable path или writable root и при любой ошибке validation/publication marker удаляется вместе со всеми его точными PATH-компонентами; поколения на диске не удаляются. Ошибка публикации остаётся warning и не отключает внутренний Brush registry.

Serving-процесс `sshd` публикует bridge один раз до Tokio runtime: выбранный проверенный canonical path или private relay. Тот же `Arc<Executable>` и descriptor-aware Tokio wrapper запускают все shell/exec sessions. При отсутствии discoverable descriptor и path `sshd` завершается до bind; ошибка самого bridge не фатальна.

Managed BusyBox invocation проверяет bridge symlink через `Executable::matches`, использует cache-only Index AXE Store и возвращает status applet вместо входа в новый shell. Transient fallback AXE Store пропускает все кандидаты внутри текущего `AXE_APPLET_DIR` и любой path того же executable, поэтому filesystem bridge не вызывает рекурсию.

В verbose output `doctor` явно разделяет passive `Observed restrictions` и `Active checks`.
Self-exec probe использует тот же `Executable::command()`, fork probe проверяет
создание процесса, а memory probes раздельно сообщают `memfd_create`, принятие
флага `MFD_EXEC`, anonymous RW→RX `mprotect` и macOS `MAP_JIT|RX`. Ни успешный
`memfd_create`, ни принятый `MFD_EXEC` сами по себе не доказывают возможность
создать executable mapping. Filesystem probe подтверждает создание, запись,
sync и cleanup private test file; mountinfo отдельно сообщает, установлен ли
`noexec`. Doctor не запускает тестовый shell script и не делает вывод о
filesystem execution из наличия `/bin/sh`. `Attention` включает только
отрицательные active results, `noexec` и degradations вместе с их runtime impact.
Каждый typed результат имеет
`available`, `absent`, `unavailable`, `unsupported`, `unknown`,
`not_applicable` или `redacted`; отсутствие evidence не превращается в
успешный verdict.

## Источники ввода Brush

`brush-shell` собирается с двумя явно заданными features:

- `minimal`: скрипты, пайпы и stdin без терминала;
- `reedline`: интерактивные терминальные сессии.

Brush выбирает Reedline, когда stdin подключён к терминалу и shell работает интерактивно. Если убрать feature `reedline`, код выбора останется, но сам бэкенд пропадёт. В результате shell упадёт на старте с `requested input backend type not supported`. Оставлять один `minimal` нельзя.
Асинхронный list выполняется в отдельном Tokio task с клоном состояния shell.
Если его pipeline запускает дочерний процесс, `$!` получает настоящий PID. Для
полностью in-process list отдельного безопасного OS PID нет: `$!` получает job
spec `%N`, разрешаемый builtin-ом `wait`. Это не создаёт PID, который мог бы
совпасть с чужим процессом; task привязан к lifetime текущего shell.

## Встроенные команды

В `src/applets/` лежат адаптеры для готовых переиспользуемых реализаций на Rust и локальные реализации там, где требуется особое интеграционное поведение:

- `mod.rs`: адаптеры `grep` и `sed`;
- `findutils.rs`: адаптер `find` и рекурсивный `xargs`, который умеет запускать встроенные команды;
- `diffutils.rs`: `diff`, `cmp` и ориентированный на merge `diff3`;
- `tar.rs`, `gzip.rs`, `jq.rs`: апплеты для архивов, сжатия и JSON;
- `sshd.rs`: SSH-сервис и relay client; relay server вынесен в отдельный `crates/axe-relay`;
- `vzik`: отдельный `crates/vzik` с ядром коллектора protocol-v3 и жёсткими лимитами;
- `porto-api`: полные Rust bindings канонического Porto RPC protocol и bounded Unix-socket client.

Для встроенного апплета shell не нужен отдельный вспомогательный бинарник. Рекурсивный запуск снова входит в текущий исполняемый файл через скрытый флаг Brush для dispatch встроенных команд.

У `vzik` есть отдельный бинарник и multicall entry point. Запуск без аргументов печатает корневой help; `-h`/`--help` также работает для каждой группы и конечной команды. Иерархический CLI строится через lean `clap_builder` без derive. Команда `vzik collect` запускает фиксированный полный baseline: сведения о хосте и ядре вместе с ключевыми security-настройками, процессы и их security context, сетевые адреса, соседи и данные firewall, mounts и cgroups, локальная аутентификация и sudo policy, базы нативных пакетов, static system/user unit inventory, runtime systemd и D-Bus inventory, расписания, контейнеры, конфигурация SSH и authorized keys. Обход всей файловой системы в baseline не входит; `/proc/config.gz` тоже не используется по умолчанию. Каждую capability можно выбрать отдельно. Среди них есть явный scan privilege surface в пределах одной файловой системы и точечная проверка файлов — оба с заданными лимитами.

`service.list` читает определения всех одиннадцати типов systemd unit из system, global-user и найденных через `/etc/passwd` per-user roots. `systemctl.list` и `systemctl.inspect` через `zbus` напрямую подключаются к system или user D-Bus и читают runtime unit state; `dbus.list` и `dbus.inspect` собирают метаданные шины, owned/activatable names, owners и доступные process credentials без активации сервисов. По умолчанию используется `/run/dbus/system_bus_socket`; `--user` выбирает `/run/user/UID/bus`, `--socket` задаёт точный сокет, а list-команды с `--all-users` обходят доступные user bus. Runtime-agnostic `container` строит inventory и context только по procfs, cgroups и namespaces. Porto-specific capability вынесена в `vzik portoctl`: она использует read-only Porto RPC, по умолчанию через `/run/portod.socket`, публикует каталог и bounded chunks свойств. Значения `env` скрыты по умолчанию, а `stdout` и `stderr` читаются только с явным byte limit. Путь к сокету можно заменить через `--socket`; внешние бинарники `systemctl`, `busctl` и `portoctl` не запускаются.

Единая типизированная схема capabilities задаёт дерево Clap, help, ограничения CLI и ID capabilities в JSONL. `vzik capabilities` сериализует компактный индекс с protocol semantics, global limits и стабильными ID; `vzik capabilities CAPABILITY_ID` добавляет request schema, data kinds, access class и возможные outcomes только для выбранной capability. Отдельной параллельной схемы discovery и эвристического выбора capability нет. Коллектор не использует shell, `PATH`, дочерние процессы и INET-сокеты; systemd, D-Bus и Porto capabilities создают только bounded Unix connections к выбранным сокетам. Недоступность отдельного manager или bus даёт diagnostic и terminal capability outcome `unavailable` или `partial`, не прекращая остальной collection.

Protocol-v3 начинает поток `stream_start` с полным и уникальным `planned_capabilities`. `stream_end` содержит `not_started_capabilities` и stream outcome: `complete` означает, что каждая запланированная capability завершилась с outcome `complete`; любое `partial`, `unavailable`, `unsupported` или незапущенная capability даёт `degraded`. Эти terminal semantics согласованы с process status: `0` для complete, `3` для degraded. Прерывание или cooperative deadline завершается `stream_abort` и ненулевым process status, но не успешным summary. Проверки cancellation выполняются между bounded acquisition operations, поэтому блокирующая системная операция может задержать наблюдение сигнала. Приёмник вывода заранее резервирует место для terminal records и ограничивает размер строки, всего потока и число записей. Process-level diagnostics в stderr используют schema version 3 и имеют стабильные `code`, `operation`, `retryable`, `message` и `details`.

## AXE Store

В корне лежит виртуальный Cargo workspace. `axe-artifact` владеет wire-схемой metadata v2 (Index, manifest и подписанный envelope) с контекстом Ed25519ph `axe-metadata-v2`; формат и контекст подписанного payload/trailer остаются v1. `axe-store` формирует артефакты, подписывает S3-запросы компактным SigV4 signer и отправляет их синхронным HTTP-клиентом. `axe-store-client` читает артефакты и линкуется в `axe`; типы Nix, S3 signer и publisher HTTP transport до клиента не добираются.

Корневые `flake.nix`, `flake.lock` и модули категорий в `store/nix/packages/` задают public список пакетов и граф сборки. Общие конструкторы и вычисление ID лежат в `store/nix/packages/lib.nix`; `lib.axeStore.mkPackageSet` принимает edition-specific CA bundle и дополнительные category attributes. Локальные patches и статические inputs public package set находятся рядом в `store/nix/`. CGO-free Go tools для `aarch64-darwin` строятся из Linux package derivations с `GOOS=darwin` и `GOARCH=arm64`, поэтому не требуют native Darwin builder и не получают store-local `libresolv`. Остальные Darwin packages сохраняют свой target-native build contract. Nix отвечает за зафиксированные inputs, загрузки и сборку из исходников, sandboxing, кэши и удалённые сборщики. `axe-store` сначала вычисляет output paths выбранных package-target derivations и только потом строит их; target-local failure помечается unsupported, не отменяя остальные targets.

`axe-store` публикует под prefix выбранной edition (`store` у OSS, прежний `tools` не перезаписывается):

- `objects/sha256/<first-two>/<full-sha256>`: сжатый payload и фиксированный skippable frame Zstandard с подписью;
- `tools/<id>/manifests/<sha256>.cbor`: неизменяемый подписанный manifest, digest всех подписанных байт закреплён в записи Index;
- `index.cbor.zst`: сжатый подписанный discovery Index для клиентов;
- `index.json`: читаемое представление, не используемое клиентами.

Порядок публикации: objects → manifests → CBOR Index (CAS) → JSON Index. Неизменяемые объекты и manifests в S3 записываются с `If-None-Match: *`; при конфликте сравнивается SHA-256 всех байт. Перед CAS publisher проверяет подписанные staged Index и manifests, соответствие digest и ID. При создании CBOR Index используется `If-None-Match: *`, при обновлении — ранее прочитанный ETag в `If-Match`. Поэтому параллельный publisher не может изменить manifest уже принятого поколения. JSON публикуется после успешного CBOR CAS и обновляется только вперёд по generation. До публикации проверяется, что ранее опубликованные `package`, `channel` или `target` не исчезли. Удаление разрешается только с явно переданным `--allow-target-removal`.

Сам `axe` публикуется отдельным unsigned transport layer поверх уже проверенного
release binary: `axe/releases/<cargo-version>-<sha256>/<target>/axe` immutable,
`axe/stable/<target>/axe` mutable. Publisher
сначала валидирует формат, архитектуру и static Linux invariant всех выбранных
артефактов, затем загружает все immutable objects, обновляет stable objects и
атомарно записывает `nix/axe-releases.json`. Корневой flake использует только
versioned URL и SRI hash из этого файла; stable path не участвует в
воспроизводимой Nix-сборке. Полный `store-sync` сначала генерирует inventory,
публикует Store Index и атомарно копирует подписанный staged Index в
`store/bootstrap-index.cbor.zst` выбранной edition, затем строит `axe`,
обновляет `web/data/registry.json` из собранного Linux release binary и
публикует stable `axe` последним. Отдельный `axe-release` требует уже готовый
подписанный snapshot; development build без snapshot не встраивает Index.

Во время работы `crates/axe/src/ondemand.rs` делегирует всё `axe-store-client`:

1. объединяет встроенный подписанный bootstrap Index с проверенным Index из кэша или сети; digest manifest проверяется до подписи и до использования сохранённых байт;
2. применяет `AXE_STORE_MODE`: `auto` перепроверяет протухшие метаданные через ETag, `cache-only` запрещает Index, manifest и object HTTP, `off` исключает Store entries из runtime registry;
3. выбирает только настроенный `channel` и текущий `target`;
4. полностью проверяет размер, SHA-256, ключ trailer-а и подпись нового или изменившегося объекта; после успешной проверки сохраняет state рядом с payload в приватном content-addressed directory;
5. при повторном использовании сравнивает fingerprint inode и metadata объекта или дерева; совпадение не требует чтения payload, а изменение, отсутствующий/corrupt state или восстановление после прерванной операции запускает полную проверку;
6. под digest-scoped lock атомарно публикует сжатые объекты и распакованные payload-ы целыми directories: сначала в `$AXE_STORE_DIR`, затем в платформенном кэше, на доступных для записи mount-ах, в tmpfs и, наконец, во временном хранилище;
7. если файловая система недоступна, запускает одиночные Linux-бинарники из sealed `memfd`; пакетам нужно проверенное дерево на файловой системе;
8. сохраняет Index, manifests и network backoff под `<root>/metadata/<namespace>/`, где namespace вычислен из нормализованного effective Store URL, отсортированных trusted key IDs и channel. Если edition-specific `AXE_STORE_DIR` недоступен и разные editions попали в общий fallback root, их metadata и backoff не смешиваются; content-addressed objects могут лежать рядом, но verified state другой trust identity требует новой проверки. `clean-tools` удаляет только metadata текущего namespace, не общие objects и не соседние editions;
9. сохраняет семантику статуса и сигнала дочернего процесса, а прогресс показывает только при реальной загрузке.

Сборка `capsh` для AXE Store открывает выбранный filesystem path из `AXE_SHELL` до изменения capabilities или root и для обычного `--` вызывает его через `fexecve`. Это безопасный default там, где backend поддерживается. `-+` использует тот же original-or-relay path из `SHELL` через launcher libcap. Явный path сохраняет обычную семантику upstream.

Внешний кандидат из `PATH` разрешён только при временных ошибках сети или хранилища, включая cache miss в `cache-only`. Ошибки целостности, схемы, подписи, хеша и TLS завершаются с кодом 126. Кандидаты внутри `AXE_APPLET_DIR` и path того же executable пропускаются, чтобы не зациклиться через proc, filesystem symlink или hardlink. Скрытый re-exec встроенной команды не обновляет Index; при построении видимого пользователю реестра протухшие метаданные можно перепроверить в `auto`. `--help`, `--version` и прямой запуск bundled applet не инициализируют AXE Store; `sshd` загружает Store registry без обновления Index для PATH bridge.

## Модель доверия SSH

`crates/axe/src/applets/sshd.rs` встраивает ключи и настройки доверия во время сборки:

- `config/sshd.json`: разрешённые по умолчанию principals SSH-сертификатов;
- `keys/ssh/user_ca_keys`: публичные пользовательские CA OpenSSH;
- `keys/ssh/host_ed25519`: идентичность сервера.

Аутентификация принимает только пользовательские сертификаты OpenSSH, которые:

- указывают username из актуального списка разрешённых principals;
- содержат ровно этот username как principal;
- не содержат critical options.

`config/sshd.json` задаёт встроенный список по умолчанию. Во время запуска его можно целиком заменить через `--principals alice,bob` или `AXE_SSHD_PRINCIPALS=alice,bob`; CLI имеет приоритет над окружением.

Без `--workdir` сервер использует process-wide runtime root; явный `--workdir` или `AXE_WORK_DIR` имеет приоритет. Если path AXE Store не задан явно, `sshd` создаёт `<workdir>/.axe-store`. Выбранные `AXE_WORK_DIR`, optional `AXE_STORE_DIR` и эффективный `AXE_STORE_MODE` фиксируются до listener и передаются каждой shell/exec session. `--store-mode` может только ужесточить inherited environment policy. В `off` derived Store directory не создаётся, Store entries не попадают в registry дочерних shell и серверный PATH bridge содержит только bundled commands.

В режиме `auto` listener владеет единственным Index refresher. Он запускает `RevalidateIfStale` после readiness и при переходе от одной активной shell/exec session к нулю; SFTP и forwarding в счётчик не входят. Session child читает только последний verified Index, сохраняя `auto` для последующей доставки Store tool. Новая session не ждёт и не отменяет уже начатый refresh. Непрерывная нагрузка может откладывать автоматическое обновление без ограничения; явный `refresh-tools` остаётся доступен.

PID-файлы, историю и конфигурацию `sshd` не создаёт. Lifecycle events уходят в stdout/stderr процесса; `--daemon` перенаправляет их в `--log-file`, а без флага — в null. На Linux descriptor-aware Tokio launcher позволяет новым `exec` и `shell` sessions запускать Brush после unlink/replace даже без procfs; если descriptor syscalls запрещены, используется заранее выбранный checked original-or-relay path. На Darwin используется проверенный filesystem path. Если ни descriptor, ни path не обнаружены, сервер возвращает status 1 с `sshd: cannot locate AXE executable for shell sessions` до bind. Read-only explicit workdir допустим: descriptor launch не требует relay, а ошибка автоматического создания каталога AXE Store не инвалидирует embedded host identity.

Закрытие SSH-канала закрывает его stdin; после ошибки транспорта вывод дочернего процесса продолжают вычитывать, чтобы задача завершилась без блокировки на pipe. SFTP channel принадлежит SSH session: сервер ждёт или отменяет его при EOF/close, принимает packets до 256 КиБ, ограничивает transfer 240 КиБ и хранит до 256 open handles. Relay registration переподключается с ограниченным jittered backoff. `--relay-transport tcp|quic` задаёт control transport; default — `tcp`. Listener `sshd` владеет relay task и завершает её при shutdown.

Фабрика russh создаёт lifecycle guard для каждого TCP transport. Guard считает активные transport-ы, запоминает principal после certificate-аутентификации и при drop пишет peer, principal, длительность и остаток активных transport-ов. Clean EOF, protocol error и ошибка setup завершаются одним disconnect event. Russh закрывает transport после часа без protocol activity.

Connection-local handler отмечает welcome после первого успешно запущенного PTY shell и добавляет строку перед его stdout. Состояние живёт ровно столько же, сколько TCP transport: multiplexed shell channels не повторяют welcome, а remote exec, shell без PTY, SFTP и forwarding его не получают.

`sshd` ограничивает Tokio runtime доступным CPU. На Linux он читает `cpu.max` текущего cgroup v2 и каждого родительского cgroup, берёт минимальный quota и сопоставляет дробную квоту хотя бы одному worker-у; CPU affinity остаётся верхней границей. При одном worker-е используется current-thread runtime, чтобы контейнер с долей CPU не создавал по потоку на каждое host CPU и не тратил квоту на лишнее планирование.

## Relay и супервизор

Сервер relay живёт в отдельном бинарнике `axe-relay`; `axe` содержит только клиент `sshd` для исходящего подключения с хоста за NAT. Сервер одновременно поднимает TCP control listener и UDP QUIC endpoint, затем выделяет публичные TCP listener-ы из общего настроенного диапазона портов. `--public-bind` выбирает локальный IP интерфейса, а обязательный `--public-host` — внешний DNS/IP, передаваемый клиенту как `public_address` с выделенным портом; relay за NAT требует проброс диапазона и доступность этого адреса с машины оператора. HTTP dashboard/API слушает только loopback, удалённый доступ идёт через аутентифицированный HTTPS reverse proxy. `GET /api/v1/status` возвращает версионированный read-only snapshot: uptime, адреса control listener-ов и активные регистрации (client ID, transport, peer, public address, duration). Тот же snapshot используют dashboard и CLI `axe-relay status|clients`; записи о клиентах удаляются при drop connection handler-а. Сервер запускается foreground под внешним supervisor; готовность HTTP API появляется после запуска control listeners.
CLI `axe-relay wait --client-id ID --transport tcp|quic --timeout SEC` периодически читает текущий snapshot до совпадения и возвращает `public_address`; `--after-id N` исключает прежнюю регистрацию с тем же ID. `axe-relay watch [--client-id ID]` сначала выводит активных клиентов как `present`, затем поток `connected`/`disconnected` (с `--json` — JSONL). `GET /api/v1/events` атомарно выдаёт snapshot и курсор с UUID эпохи relay; запрос `?epoch=UUID&after=N` читает накопленные события. Журнал ограничен 128 записями, а пропуск из-за переполнения или смены эпохи даёт HTTP 409: watch завершается с ошибкой вместо ложной полноты фида. Агент получает адрес из API через HTTPS proxy и подключается по SSH с сертификатом; регистрация не подтверждает работу data plane. `just relay-live-smoke` проверяет wait, watch и SSH banner для обоих transport-ов локально, без доказательства внешней доступности.

TCP registration использует один versioned CBOR-фрейм с лимитом размера и префиксом длины. Затем то же TCP-соединение несёт бинарный data plane [yamux](https://github.com/hashicorp/yamux/blob/master/spec.md). Каждое публичное TCP-соединение соответствует одному двунаправленному потоку yamux. TCP keepalive, лимит в 128 потоков и общее окно приёма в 32 МиБ ограничивают время жизни мёртвых сессий и потребление памяти. TCP-аутентификация сравнивает token не короче 32 байт за константное время; сервер читает `AXE_RELAY_TOKEN` только в runtime, а `AXE_RELAY_TOKEN` у `sshd` переопределяет optional embedded token.

QUIC transport аутентифицирует обе стороны через TLS 1.3 с раздельными identities. Relay предъявляет server certificate и читает соответствующий private key из `--quic-key`; standalone `axe-relay` читает оба сертификата из runtime файлов, не встраивая private trust. `sshd` доверяет server certificate и предъявляет client certificate/private key. Edition может встроить certificates и client key в `axe`. Если они не встроены, runtime paths задаются через `AXE_RELAY_QUIC_SERVER_CERT_FILE`, `AXE_RELAY_QUIC_CLIENT_CERT_FILE` и `AXE_RELAY_QUIC_CLIENT_KEY_FILE`. Первый bidirectional stream несёт versioned CBOR registration/response; каждое публичное TCP-соединение получает отдельный reliable bidirectional QUIC stream.

`config/relay.json` содержит `enabled_by_default` и nullable TCP/QUIC endpoint-ы. `--relay` всегда включает выбранный transport, `--no-relay` всегда выключает и конфликтует с `--relay`. Без override relay task создаётся только при включённом edition default. Endpoint и credentials проверяются до bind/readiness; пустая строка не является disable-маркером.

QUIC keepalive равен 5 секундам, idle timeout — 30 секундам; это ограничивает обнаружение недоступного relay после обрыва без отдельного heartbeat-протокола. Migration поддерживает NAT rebinding и смену client address. Per-stream receive window равен 16 МиБ, connection receive/send windows — 64 МиБ, peer может открыть до 1024 bidirectional streams. BBR управляет congestion window при потерях пакетов; data path напрямую проксирует stream.

Оба control listener-а используют общий предел в 256 одновременных connection handlers, требуют registration за 10 секунд и хранят handler tasks в одном `JoinSet` до shutdown. После принятой registration relay создаёт lifecycle guard. Он считает только аутентифицированные управляющие соединения и при любом выходе handler-а пишет transport, `client_id`, peer, public address, длительность и остаток активных clients.

`sshd` поддерживает `--daemon`: текущий бинарник запускается заново как супервизор. Launcher возвращает success только после bind воркера через inherited readiness descriptor. Супервизор владеет worker process group, пересылает TERM/INT/QUIT/HUP, ждёт graceful shutdown и только затем использует SIGKILL; Linux worker получает parent-death signal. После ошибок супервизор применяет ограниченную задержку повторного запуска с jitter, а после стабильной работы сбрасывает счётчик ошибок. Если лог создать не удалось, вывод уходит в null, но сервис всё равно запускается. `axe-relay` использует внешний supervisor (например systemd) и завершает control tasks при SIGTERM/INT.

## Встраивание во время сборки

`crates/axe/build.rs` разделяет source workspace и выбранный edition root,
строго проверяет bundle, а затем генерирует типизированные константы в
`OUT_DIR`. Source workspace владеет `crates/axe/src/help.txt` и
`config/aliases.json`; edition root владеет:

- `edition.json`, `config/store.json`, `config/relay.json`, `config/sshd.json`;
- `keys/ssh/host_ed25519`, `keys/ssh/user_ca_keys` и optional relay credentials,
  если edition настраивает соответствующий endpoint;
- отсортированным списком `keys/store/trusted/*.pub`;
- сгенерированным `store/bootstrap.json` и, для release, подписанным `store/bootstrap-index.cbor.zst`;
- optional `store/nix/assets/trusted_ca.pem`, который читает
  `axe-tls-roots/build.rs`.

Private signing key AXE Store и S3 credentials не читаются при сборке `axe`.
Build проверяет подписанный snapshot по trusted public keys, wire-схеме и
сгенерированному inventory; обычная development-сборка без snapshot допустима,
release recipe его требует. Отсутствующий обязательный input не ищется в source
workspace: build завершается ошибкой с path выбранного edition root.

## Дифференциальные тесты апплетов

`crates/axe/tests/applet_parity.rs` запускает один сценарий через встроенный
апплет и через каноническую внешнюю утилиту, затем сравнивает status, signal,
stdout и stderr без UTF-8-нормализации. Для форматов с несколькими допустимыми
представлениями проверяется совместимость: AXE читает поток эталонного
компрессора, а эталонный декомпрессор читает поток AXE. Для `tar` тесты
сравнивают извлечённое дерево и проверяют архив AXE эталонным `tar`; для
динамического `ip -j` сравнивают стабильную проекцию схемы по интерфейсам и
адресам.

Эталонные бинарники задаёт dev shell через `AXE_REFERENCE_PATH`; тест не ищет
случайные host-утилиты. Обоим процессам задаются одинаковые `C` locale, timezone,
рабочий каталог и `PATH`. Первый слой покрытия намеренно относится к локальным
реализациям (`jq`, `strings`, `file`, `xargs`, compression, `tar`, `blkid`,
`ip`); адаптеры готовых upstream-наборов расширяются после них.

`xargs` разбирает stdin инкрементально, формирует batches в пределах `_SC_ARG_MAX` с запасом на environment и ограничивает `-P0` числом доступных CPU (не больше 256). `strings --parallel` передаёт output каждого worker-а через bounded channels по 64 КиБ и вычитывает их в порядке файлов; память не растёт пропорционально суммарному output.

## Статические артефакты Linux

`.cargo/config.toml` включает `crt-static` и static relocation model для обеих
целей musl, запрещает PIE через `-no-pie` и явно выбирает stable `ld.lld`
flavor. Тонкий `tools/rust-lld` запускает linker из активного закреплённого Rust
toolchain без изменения аргументов или CRT objects.

Linux release recipes всегда перелинковывают конечный executable, проверяют ELF-инварианты до копирования в `dist/` и smoke-запускают нативный amd64 artifact с `--list`.

Корректный Linux-артефакт:

- имеет ELF-тип `EXEC`;
- не содержит интерпретатора программы;
- не содержит тегов `NEEDED` для динамических библиотек;
- успешно выполняет сценарии со встроенными командами при пустом или несуществующем `PATH`.

Darwin использует платформенный линкер; ELF-инварианты к нему не относятся.
