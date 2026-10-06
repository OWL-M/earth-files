earth-files = Earth Files
empty-folder = Празна папка
empty-folder-hidden = Празна папка (съдържа скрити елементи)
no-results = Няма намерени резултати
filesystem = Файлова система
home = Домашна папка
networks = Мрежи
notification-in-progress = Файлови операции са в процес на изпълнение.
trash = Кошче
recents = Скоро ползвани
search-title = Търсене "{$term}": {$name}
undo = Отменяне
today = Днес
# Desktop view options
# List view
name = Име
modified = Променян
trashed-on = Изтрит
size = Размер
type-heading = Вид
file-type-folder = Папка
file-type-image = Изображение
file-type-video = Видео
file-type-audio = Аудио
file-type-text = Текст
file-type-archive = Архив
file-type-document = Документ
file-type-program = Програма
file-type-other = Друго
# Progress card
details = Подробности
pause = Пауза
resume = Продължаване
tasks-count = { $count } { $count ->
    [one] задача
   *[other] задачи
  }
tasks-more = още { $count }
task-loading = Зареждане на { $name }
task-mounting = Монтиране на { $name }
task-unmounting = Демонтиране на { $name }
task-done = Готово
task-failed = Неуспешно
task-paused = На пауза
task-paused-waiting = На пауза, изчаква
blocked-read = „{$name}“ не може да бъде прочетен
blocked-list = Папката „{$name}“ не може да бъде отворена
blocked-remove = „{$name}“ е копиран, но не може да бъде премахнат
same-for-rest = Същото за останалите
retry = Опитайте отново
retry-as-root = Опитайте отново като администратор
use-root-again = Използвайте отново администраторските права
root-not-granted = Не е предоставен администраторски достъп
abort = Спиране
blocked-failed = „{$name}“ не може да бъде завършен
abort-tooltip = Спиране тук и запазване на свършеното
cancel-tooltip = Спиране и връщане на всичко обратно
stalled-title = Устройството не отговаря
stalled-for = Няма отговор от {$seconds} с
keep-original = Запазване на оригинала
destination-no-permission = Не може да се пише в „{$folder}“: няма права
destination-read-only = Не може да се пише в „{$folder}“: само за четене
blocked-link = „{$name}“ е връзка, а това устройство не може да съдържа връзки
blocked-link-fs = „{$name}“ е връзка, а това устройство ({$fs}) не може да съдържа връзки
blocked-too-big = „{$name}“ е твърде голям за това устройство ({$fs})
blocked-bad-name = „{$name}“ съдържа знаци, които това устройство ({$fs}) не може да съхрани
blocked-delete = Нямате разрешение да изтриете „{$name}“
blocked-no-trash = „{$name}“ е на устройство без кошче
delete-permanently-as-root = Изтриване завинаги като администратор
deleted-for-good = {$more ->
    [0] Отменена, но „{$name}“ вече е изтрит завинаги
    *[other] Отменена, но „{$name}“ и още {$more} вече са изтрити завинаги
  }
progress-asking = на пауза
failed-path = „{$name}“: {$reason}
reason-no-permission = няма права
reason-drive-full = устройството е пълно
reason-read-only = устройството е само за четене
reason-gone = вече не съществува
reason-too-big = твърде голям е за това устройство
blocked-move = „{$name}“ не може да бъде преместен
blocked-move-reason = Оригиналите не могат да бъдат премахнати от „{$folder}“: {$reason}
same-for-rest-count = Същото за останалите ({$count})
not-enough-space = Нужни са {$needed}, но свободни са само {$free}
in-use = „{$name}“ се използва от друга операция: {$operation}
checking = Проверка… файлове: {$files}, {$size}
rollback-failed = {$more ->
    [0] Отменена, но „{$name}“ не може да бъде върнат на мястото си
    *[other] Отменена, но „{$name}“ и още {$more} не могат да бъдат върнати на местата си
  }
failed-operations-title = {$count ->
    [one] Операцията е неуспешна
    *[other] {$count} операции са неуспешни
  }

# Dialogs


## Compress Dialog

create-archive = Създаване на архив

## Extract Dialog

extract-password-required = Необходима е парола
extract-as-folder = Разархивиране в папка
extract-to = Разархивиране в...
extract-to-title = Разархивиране в папка

## Empty Trash Dialog

empty-trash = Изпразване на кошчето
empty-trash-warning = Сигурни ли сте, че искате да изтриете завинаги всички елементи в кошчето?
empty-before-eject-title = Изпразване на кошчето преди изваждане?
empty-before-eject-body = Изпразнете кошчето, за да освободите място на „{$name}“. Всички елементи в кошчето ще бъдат изтрити окончателно.
do-not-empty = Без изпразване

## Mount Error Dialog

mount-error = Устройството не може да бъде достъпно

## New File/Folder Dialog

create-new-file = Създаване на нов файл
create-new-folder = Създаване на нова папка
file-name = Име на файла
folder-name = Име на папката
file-already-exists = Вече съществува файл с това име.
folder-already-exists = Вече съществува папка с това име.
name-hidden = Имената, започващи с „.“, ще бъдат скрити.
name-invalid = Името не може да бъде „{ $filename }“.
name-no-slashes = Името не може да съдържа наклонени черти.

## Open/Save Dialog

cancel = Отказване
create = Създаване
open = Отваряне
open-file = Отваряне на файл
open-folder = Отваряне на папка
open-in-new-tab = Отваряне в нов раздел
open-in-new-window = Отваряне в нов прозорец
open-item-location = Отваряне на местоположението на обекта
open-multiple-files = Отваряне на няколко файла
open-multiple-folders = Отваряне на няколко папки
save = Запазване
save-file = Запазване на файла

## Open With Dialog

open-with-title = Как искате да отворите „{ $name }“?
browse-store = Разгледайте { $store }
other-apps = Други програми
related-apps = Свързани програми

## Permanently delete Dialog

selected-items = избраните { $items } елемента
permanently-delete-question = Изтриване завинаги
delete = Изтриване
permanently-delete-warning = Сигурни ли сте, че искате да изтриете завинаги { $target }? Това действие не може да бъде върнато.

## Rename Dialog

rename-file = Преименуване на файла
rename-folder = Преименуване на папката

## Batch Rename Dialog
batch-rename-title = Преименуване на {$count} елемента
batch-rename-template = Шаблон
batch-rename-find-replace = Търсене и замяна
batch-rename-new-name = Ново име
batch-rename-tag-name = [Оригинално име]
batch-rename-tag-number = [1, 2, 3]
batch-rename-add-name = Добавяне на оригиналното име
batch-rename-add-number = Добавяне на номер
find = Търсене
batch-rename-conflicts = {$count ->
    [one] Едно ново име е в конфликт със съществуващ елемент или друго ново име
    *[other] {$count} нови имена са в конфликт със съществуващи елементи или помежду си
  }

## Replace Dialog

replace = Заменяне
replace-title = „{ $filename }“ вече съществува на това местоположение.
replace-warning = Искате ли да го замените с този, който запазвате? Ако го замените, ще презапишете съдържанието му.
replace-warning-operation = Искате ли да го замените? Ако го замените, ще презапишете съдържанието му.
original-file = Съществуващ файл
replace-with = Замяна с
apply-to-all = Прилагане за всички
keep-both = Запазване на и двата
merge = Обединяване
replace-folder-warning = Искате ли да ги обедините, или да замените папката, която е там? Ако я замените, тя ще бъде преместена в кошчето.
folder-totals = { $files ->
    [one] { $files } файл, { $size }
   *[other] { $files } файла, { $size }
}
skip = Пропускане

## Set as Executable and Launch Dialog

set-executable-and-launch = Задаване като изпълним и стартиране
set-executable-and-launch-description = Искате ли да зададете „{ $name }“ като изпълним и да го стартирате?
set-and-launch = Задаване и стартиране
launch-desktop-entry = Launch application?
launch-desktop-entry-description = "{$name}" is not an installed application. Launching it runs the command below.
launch-anyway = Launch

## Metadata Dialog

open-with = Отваряне с
owner = Собственик
group = Група
other = Друго

### Mode 0

none = Без

### Mode 1 (unusual)

execute-only = Само за изпълняване

### Mode 2 (unusual)

write-only = Само за записване

### Mode 3 (unusual)

write-execute = Записване и изпълняване

### Mode 4

read-only = Само за четене

### Mode 5

read-execute = Четене и изпълняване

### Mode 6

read-write = Четене и записване

### Mode 7

read-write-execute = Четене, записване и изпълняване

## Favorite Path Error Dialog

favorite-path-error = Грешка при отваряне на папката
favorite-path-error-description =
    Местоположението „{ $path }“ не може да бъде отворено.
    Възможно е да не съществува или да нямате права да го отворите.

    Искате ли да го премахнете от страничната лента?
remove = Премахване
keep = Запазване

# Context Pages


## About

repository = Хранилище
support = Поддръжка

## Add Network Drive

add-network-drive = Добавяне на мрежово устройство
network-drives-connected = { $count } { $count ->
    [one] свързано
   *[other] свързани
  }
connect = Свързване
connect-anonymously = Свързване анонимно
connecting = Свързване...
domain = Домейн
enter-server-address = Въведете адрес на сървър
network-drive-description =
    Адресите на сървърите включват представка на протокола и адрес.
    Примери: ssh://192.168.0.1, ftp://[2001:db8::1]

### Make sure to keep the comma which separates the columns

network-drive-schemes =
    Налични протоколи,Представка
    AppleTalk,afp://
    Протокол за пренос на файлове,ftp:// или ftps://
    Мрежова файлова система,nfs://
    Server Message Block,smb://
    Пренос на файлове по SSH,sftp:// или ssh://
    WebDav,dav:// или davs://
network-drive-error = Мрежовото устройство не може да бъде достъпно
password = Парола
remember-password = Запомняне на паролата
try-again = Опитайте отново
username = Потребителско име

## Operations

cancelled = Отменена
operation-failed-to-start = The file operation could not be started
edit-history = Редактиране на историята
history = История
no-history = Няма елементи в историята.
pending = Чакащи
progress = { $percent }%
progress-cancelled = { $percent }%, отменена
progress-failed = { $percent }%, неуспешно
progress-paused = { $percent }%, на пауза
failed = Неуспешна
complete = Завършена
compressing =
    Компресиране на { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“ ({ $progress })...
compressed =
    Компресирано е { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“
copy_noun = Копиране
creating = Създаване на „{ $name }“ в „{ $parent }“
created = „{ $name }“ е създадено в „{ $parent }“
copying =
    Копиране на { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“ ({ $progress })...
copied =
    Копирано е { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“
deleting =
    Изтриване на { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от { trash } ({ $progress })...
deleted =
    Изтрито е { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от { trash }
emptying-trash = Изпразване на { trash } ({ $progress })...
emptied-trash = { trash } е изпразнено
extracting =
    Извличане на { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“ ({ $progress })...
extracted =
    Извлечено е { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“
setting-executable-and-launching = Задаване на „{ $name }“ като изпълним и стартиране
set-executable-and-launched = „{ $name }“ е зададен като изпълним и е стартиран
setting-permissions = Задаване на правата за { $name }" на { $mode }
set-permissions = Правата за "{ $name }" бяха зададени на { $mode }
moving =
    Преместване на { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“ ({ $progress })...
moved =
    Преместено е { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от „{ $from }“ в „{ $to }“
permanently-deleting =
    Изтриване завинаги на { $items } { $items ->
        [one] елемент
       *[other] елемента
    }
permanently-deleted =
    Изтрито е завинаги { $items } { $items ->
        [one] елемент
       *[other] елемента
    }
removing-from-recents =
    Премахване на { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от { recents }
removed-from-recents =
    Премахнато е { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от { recents }
renaming = Преименуване на „{ $from }“ на „{ $to }“
renamed = „{ $from }“ е преименувано на „{ $to }“
renaming-many = Преименуване на { item-count }
renamed-many = Преименувани: { item-count }
restoring =
    Възстановяване на { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от { trash } ({ $progress })...
restored =
    Възстановено е { $items } { $items ->
        [one] елемент
       *[other] елемента
    } от { trash }
unknown-folder = неизвестна папка

## Open with

menu-open-with = Отваряне с...
default-app = { $name } (стандартно)

## Show details

show-details = Показване на подробностите
type = Вид: { $mime }
items = Елементи: { $items }
item-count = {$count ->
        [one] {$count} елемент
       *[other] {$count} елемента
    }
item-size = Размер: { $size }
item-created = Създаден: { $created }
item-modified = Променян: { $modified }
item-accessed = Достъпен: { $accessed }
calculating = Изчисляване...

## Settings

settings = Настройки
single-click = Отваряне с едно натискане

### Appearance

appearance = Външен вид
theme = Тема
match-desktop = Системна тема
dark = Тъмна тема
light = Светла тема

### Type to Search

type-to-search = Въведете текст за търсене
type-to-search-recursive = Търсене в текущата папка и всички подпапки
type-to-search-enter-path = Въвежда пътя до папката или файла
# Context menu
add-to-sidebar = Добавяне към страничната лента
compress = Компресиране
delete-permanently = Изтриване завинаги
eject = Изваждане
extract-here = Разархивиране
new-file = Нов файл...
new-folder = Нова папка...
open-in-terminal = Отваряне в терминала
move-to-trash = Преместване в кошчето
restore-from-trash = Възстановяване от кошчето
trash-original-location = Първоначално местоположение: {$location}
trash-original-unknown = Неизвестно
remove-from-sidebar = Премахване от стр. лента
removed-from-sidebar = { $name } е премахнат от страничната лента
remove-from-recents = Премахване от скорошни

## Desktop


# Menu


## File

file = Файл
new-tab = Нов подпрозорец
new-window = Нов прозорец
reload-folder = Презареждане на папката
rename = Преименуване...
close-tab = Затваряне на подпрозореца
quit = Спиране на програмата

## Edit

edit = Редактиране
cut = Отрязване
copy = Копиране
paste = Поставяне
select-all = Избор на всички
image-load-error = ⚠ {$error}
loading-full-image = Loading higher resolution...

## View

zoom-in = Увеличаване
default-size = Стандартен размер
zoom-out = Намаляване
view = Изглед
grid-view = Изглед като решетка
list-view = Изглед като списък
show-hidden-files = Показване на скритите файлове
show-type-column = Показване на колоната за вид
list-directories-first = Изброяване първо на папките
gallery-preview = Изглед като галерия
menu-settings = Настройки...
menu-about = Относно „{ earth-files }“...

## Sort

sort = Подреждане
sort-a-z = А→Я
sort-z-a = Я→А
sort-newest-first = Най-новите първи
sort-oldest-first = Най-старите първи
sort-smallest-to-largest = Най-малките до най-големите
sort-largest-to-smallest = Най-големите до най-малките
sort-type-a-z = Вид А→Я
sort-type-z-a = Вид Я→А

close = Затваряне

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
