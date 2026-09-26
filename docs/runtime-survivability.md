# Как AXE переживает unlink, отсутствие procfs и запреты на exec

У AXE есть два уровня выполнения:

- **in-process entry dispatch** — текущий процесс уже вошёл в applet через
  BusyBox-style `argv[0]`, `axe --applet`, positional command или hidden
  dispatch; сюда же относятся shell alias/function и builtin;
- **self-exec** — новый процесс AXE для bundled-команды из уже работающего
  Brush, вложенного запуска через `xargs`, daemon worker-а, SSH shell/exec
  session или BusyBox-style PATH bridge.

Первому уровню собственный executable не нужен. Но это не означает, что живой
Brush умеет выполнить любой bundled applet без spawn: обычный applet shim
создаёт дочерний AXE process. Native callback `doctor` выполняется в shell
process и получает snapshot live shell state. При потере self-exec shell
сохраняет builtins, aliases и functions.

При запуске нового процесса AXE сохраняет:

- исходный `argv[0]` applet-а;
- byte-oriented `OsString`-аргументы и non-UTF-8 paths;
- окружение после всех изменений caller-а;
- stdin/stdout/stderr и shell redirections;
- session/process-group setup тех launch boundary, которые его явно задают;
- порядок caller `pre_exec` hooks;
- Tokio `kill_on_drop` и lifetime дочернего процесса.

## Что AXE сохраняет при старте

`crates/axe/src/executable.rs` создаёт единственного process-wide владельца
self-exec capabilities и runtime root. Это происходит до сборки registry,
запуска Tokio runtime и появления других потоков.

На Linux AXE последовательно ищет:

1. inherited descriptor из внутреннего handoff предыдущего процесса AXE;
2. `/proc/self/exe`, если procfs действительно работает;
3. filesystem candidate из `current_exe()`, а затем из байтов `AT_EXECFN`,
   разрешённых относительно startup current directory;
4. private filesystem relay, если до него дойдёт дело.

Descriptor и path привязаны к device/inode текущего image. Filesystem candidate
принимается только после canonicalization и проверки, что это executable regular
file. Если descriptor уже известен, path с другим inode отбрасывается.

Inherited descriptor и runtime root не принимаются на веру: AXE проверяет живой
fd и identity, а root повторно проверяет перед использованием. Descriptor сразу
получает `FD_CLOEXEC`; внутренние environment variables handoff-а удаляются до
запуска потоков и наружу не торчат.

Здесь есть фундаментальное стартовое окно. Если Linux запустил AXE только по
pathname, procfs недоступен, а pathname удалили до того, как `initialize()`
успел открыть исходный inode, восстановить этот inode уже невозможно. Полную
гарантию против такого окна даёт только executable fd, унаследованный от
launcher-а.

Retained descriptor AXE readable. Это сильнее минимально необходимого exec-only
capability и позволяет создать relay. `O_PATH`/exec-only descriptor не
моделируется как независимая capability: возможность исполнить inode не
считается доказательством, что его байты можно прочитать и скопировать.

На Darwin descriptor backend нет. Там capability — это проверенный canonical
filesystem path.

## Как AXE запускает себя на Linux

Backend-ы bundled child идут в фиксированном порядке:

1. **`execveat(fd, "", ..., AT_EMPTY_PATH)`** — прямой запуск retained
   descriptor;
2. **`fexecve(fd, ...)`** — libc/POSIX fallback;
3. **один checked filesystem path** — исходный inode, если mount policy
   подтверждает execution, или private relay при `noexec`/потере original.
   При `unknown`/неподдерживаемом policy сначала пробуется relay, затем original.

`/proc/self/fd/<fd>` не передаётся Brush как простой path: его child sanitation
закрывает unrelated descriptors до `exec`, после чего pathname становится
dangling. Published bridge использует выбранный original-or-relay path; live
`/proc/<pid>/exe` используется только при отсутствии conventional path.

Ошибка одного descriptor backend-а не обрывает цепочку. Например, `fexecve` на
конкретной libc может внутри снова упереться в `execveat` или procfs — на старом
ядре и в зажатом seccomp-профиле это ожидаемо.

Перед descriptor launch AXE дублирует fd через `F_DUPFD_CLOEXEC`. Сначала
пробует диапазон от 256, чтобы не пересечься с обычными application descriptors;
при маленьком `RLIMIT_NOFILE` откатывается к диапазону от 3. Если ядро не знает
`F_DUPFD_CLOEXEC`, используется `F_DUPFD` с отдельной установкой `FD_CLOEXEC`.

Полные C-массивы `argv` и `envp` собираются в parent. Последний `pre_exec` только
снимает `CLOEXEC` и вызывает syscalls — никаких allocation и Rust
synchronization после `fork`. Перед path fallback AXE открывает fallback path,
сравнивает fd с сохранёнными device/inode и именно этот проверенный fd передаёт
следующему процессу.

Во внутренних callsite descriptor hook регистрируется последним. Новый
`pre_exec`, добавленный после него, не должен вызывать `close_range`,
`closefrom` или иначе закрывать retained fd; если такой sanitation появится,
descriptor необходимо явно включить в whitelist.

## Зачем нужен private filesystem relay

Retained descriptor подходит не всем. Relay закрывает два случая:

- kernel, libc или seccomp запрещают descriptor exec;
- consumer принимает только path и не умеет работать с retained descriptor.

Relay создаётся только на Linux и только из readable descriptor. Process-wide
runtime root выбирается один раз в порядке: явный `AXE_WORK_DIR`, проверенный
root descriptor-backed родителя, `$XDG_CACHE_HOME/axe`, Linux
`$HOME/.cache/axe` или Darwin `$HOME/Library/Caches/axe`,
`$XDG_RUNTIME_DIR/axe`, `$TMPDIR/axe-<uid>`. Автоматический root является
private directory с mode `0700`; runtime selection не перебирает mounts. Если
выбранный root имеет `noexec`, relay пробует только
фиксированные `$XDG_RUNTIME_DIR/axe` и `$TMPDIR/axe-<uid>`. Явный
`AXE_WORK_DIR` запрещает этот fallback.

Публикация устроена так:

1. AXE создаёт user-owned каталог `.axe-self` с mode `0700`; symlink и чужой
   владелец запрещены;
2. через `create_new` открывает уникальный temporary regular file с mode `0700`;
3. копирует image bounded-вызовами `read_at`, не меняя offset исходного
   descriptor;
4. повторно проверяет device/inode/size/mtime source;
5. делает `fsync` и меняет mode на `0500`;
6. читает linker-generated SHA-1 ELF build ID и публикует hard link
   `.axe-self/<40 hex>`;
7. при доступном `flock` удерживает shared lease на принятом relay до завершения
   процесса;
8. запускает hidden probe из опубликованного файла;
9. принимает path только после успешного probe.

Одинаковый ELF build ID даёт один relay независимо от inode исходного файла.
Под exclusive directory lock AXE оставляет current и один предыдущий relay;
дополнительные unlocked файлы удаляет. Relay с shared lease живого процесса
не удаляется и будет собран после освобождения lease. Если filesystem или
sandbox запрещает `flock` (`ENOSYS`, `EOPNOTSUPP`, `ENOLCK`, `EPERM` или
`EACCES`), AXE использует relay `nolock-<40 hex>`, который обычный GC не
трогает. Публикация доступна, но такие relay автоматически не удаляются.

Noexec mount, недоступный или read-only root, повреждённый файл и упавший probe
исключают этот root; после исчерпания roots при неопределённой policy выбирается
checked original path.

Relay лежит в private directory, но path execution всё равно оставляет окно
между последней inode-проверкой и `execve`. Retained descriptor сильнее. Relay
здесь не замена, а контролируемый fallback для policy и path-only consumers.

Relay сохраняет запускаемые байты AXE, но не является тем же filesystem security
object. У нового inode нет исходных file capabilities, LSM labels, security
xattrs и setuid/setgid semantics; mode намеренно фиксируется в `0500`.

Bundled executable provider заново выбирает fallback перед каждым launch. Если
original path потерян между запусками, relay готовится в parent до `fork`:
копирование и filesystem mutation внутри `pre_exec` небезопасны. Relay может
быть materialized до успешного descriptor exec, потому что path fallback должен
быть полностью подготовлен до входа в child.

## Что продолжит работать

<!-- markdownlint-disable MD013 -->

| Состояние | Bundled child / daemon / SSH session | PATH bridge | `SHELL` | `AXE_SHELL` |
| --- | --- | --- | --- | --- |
| Linux, original path executable | descriptor, затем original path | original path | original path | original path |
| Linux, original path `noexec` или потерян, relay есть | descriptor, затем relay | relay | relay | relay |
| Linux, original path policy unknown, relay недоступен | descriptor, затем checked original path | original path | original path | original path |
| Linux, original path удалён, relay нет, descriptor exec разрешён | descriptor-aware launch работает | отсутствует или ранее опубликованный bridge становится dangling | унаследованное значение | удалена |
| Linux, descriptor exec запрещён, relay и original path недоступны | новый процесс AXE не запускается; Brush builtins продолжают работу | отсутствует | унаследованное значение | удалена |
| Darwin, canonical path существует | path launch | canonical path | canonical path | canonical path |
| Darwin, path удалён или заменён | только in-process dispatch | отсутствует или dangling | унаследованное значение | удалена |

<!-- markdownlint-enable MD013 -->

`SHELL` и `AXE_SHELL` содержат только реальные публикуемые paths. AXE не
записывает туда `/dev/fd`, внутренний fd number или выдуманный path.

## Что происходит в плохом окружении

### Нет procfs

Отсутствующий `/proc`, masked proc mount или неработающий `/proc/self/fd`
отключает только proc backend. Если filesystem image читается, AXE всё равно
получает retained descriptor. После первого descriptor launch этот capability
передаётся следующим процессам без участия procfs.

### Image удалили или заменили

Открытый descriptor продолжает указывать на исходный inode после unlink и atomic
replace. Descriptor-aware launch запускает image открытого inode; filesystem
path с новым inode отклоняется.

Уже опубликованный PATH bridge на исходный path автоматически пережить unlink не
может. Если AXE стартовал без original path, для нового bridge потребуется
private relay.

### Нет `execveat` или его запрещает seccomp

`ENOSYS`, `EPERM` и другие ошибки `execveat` переводят запуск на `fexecve`, а
затем на один заранее выбранный checked path. При подтверждённом executable
mount это original; при `noexec` или потерянном original — relay. Если policy
запрещает descriptor syscalls, но разрешает обычный `execve` выбранного path,
bundled children, workers и SSH sessions продолжают работать.

Если запрещены и descriptor exec, и filesystem execution, маскировать это
внешней командой из `PATH` AXE не будет. Новый процесс завершится с status 126
или `NotFound` — точный результат зависит от caller-а.

### MDWE, W^X и запрет `memfd_create`

Self-exec не вызывает `memfd_create`, executable `mmap` или
`mprotect(PROT_EXEC)`. AXE исполняет уже executable descriptor либо обычный relay
file. Поэтому `PR_SET_MDWE`/W^X не требуют отдельного обхода, а запрет
`memfd_create` ничего здесь не меняет.

Sealed `memfd` AXE Store запускает single-file package payload и не участвует в
повторном запуске AXE.

### Нет writable executable filesystem

Если ни один runtime root нельзя создать, заполнить и исполнить, relay
отключается. Descriptor-aware Brush, supervisor и SSH launchers используют
retained descriptor через `execveat` или `fexecve`.

Path-only consumer после удаления original path запустить нельзя. AXE вернёт
контролируемую ошибку, а не подменит bundled-команду внешним executable.

### Окружение сломано

Пустой `PATH`, stale `AXE_APPLET_DIR`, неверный `AXE_WORK_DIR` и недоступный
`HOME` не выключают in-process registry. AXE удаляет stale bridge marker и его
точные PATH-компоненты. Ошибка публикации нового bridge остаётся warning:
direct entry dispatch и Brush builtins от bridge не зависят.

Descriptor handoff имеет приоритет только после проверки fd и identity.
Некорректное значение environment игнорируется и удаляется.

### Нет возможности создать process

Non-interactive/minimal Brush использует current-thread Tokio runtime и не
создаёт worker threads при старте. Reedline требует blocking pool, поэтому
interactive shell использует multi-thread runtime. Если `clone`/`fork`
возвращает `EAGAIN`, `ENOMEM` или запрещён policy, ошибка относится к конкретному
spawn; уже живой shell может выполнить следующий native builtin.

Обычные bundled applets внутри Brush относятся к process-required пути: shim
запускает новый AXE process. `doctor` — native callback и остаётся доступен без
spawn, включая вывод через shell redirection. Остальные bundled applets при
полном запрете spawn не выполняются in-process.

## Как этим пользуются разные consumers

### Brush

Bundled shim получает Linux `BundledExecutable::Descriptor` либо проверенный
filesystem path, затем собирает `SimpleCommand`. Он сохраняет `argv[0]`,
environment и redirections и добавляет descriptor exec последним перед spawn.

Builtin adapter ждёт завершения child внутри builtin и не получает pipeline PGID
из dispatcher-а. Bundled stage поэтому не гарантирует полную параллельность
pipeline и job-control semantics.

При недоступном self-exec Brush сохраняет builtins, aliases и functions;
bundled applet сообщает ошибку конкретного spawn.

### Supervisor

Daemon re-exec использует тот же `ExecutableCommand`. Internal supervisor/worker
flags, detached stdio, session setup и log files применяются до descriptor exec.
Launcher получает success только после bind воркера через inherited readiness
descriptor. Супервизор владеет worker process group, пересылает
TERM/INT/QUIT/HUP и ждёт graceful shutdown; Linux worker получает
parent-death signal, поэтому не переживает аварийный выход супервизора.

### SSHD

Один `Arc<Executable>` создаётся до bind и затем используется всеми shell/exec
sessions. Tokio wrapper переносит std command уже после окончательной подготовки
descriptor backend-а и сохраняет `kill_on_drop`.

Bridge для SSHD является best-effort: ошибка публикации не мешает
descriptor-aware session launch. Но если при старте нет ни descriptor, ни
executable path, `sshd` завершается до bind — создавать сессии ему всё равно
будет нечем.

### Path-only consumers

Обычный `execvp`, BusyBox-style symlink и внешняя launcher library принимают
только path. Им нужен live proc bridge, проверенный original path или relay. Один
retained descriptor PATH bridge не заменяет.

`capsh --` отдельно открывает path из `AXE_SHELL` до изменения capabilities и
запускает его через `fexecve`. Это работает, пока `AXE_SHELL` указывает на
исходный filesystem path.
