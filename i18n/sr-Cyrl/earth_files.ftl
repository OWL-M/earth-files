empty-folder = Празна фасцикла
empty-folder-hidden = Празна фасцикла (има скривене ставке)
filesystem = Систем датотека
trash = Отпад

# Context Pages


## Properties


## Settings

settings = Подешавања

### Appearance

appearance = Изглед
theme = Тема
match-desktop = Као систем
dark = Тамна
light = Светла
# Context menu
new-file = Нова датотека
new-folder = Нова фасцикла
move-to-trash = Премести у отпад
restore-from-trash = Врати из отпада
trash-original-location = Изворна локација: {$location}
trash-original-unknown = Непознато

# Menu


## File

file = Датотека
new-tab = Нова картица
new-window = Нови прозор
close-tab = Затвори картицу
quit = Изађи

## Edit

edit = Уреди
cut = Исеци
copy = Копирај
paste = Налепи
select-all = Изабери све
image-load-error = ⚠ {$error}
loading-full-image = Loading higher resolution...

## View

view = Приказ
grid-view = Прикажи мрежу
list-view = Прикажи списак
menu-settings = Подешавања...
earth-files = Earth Files
open-file = Отвори фајл
cancel = Прекини
repository = Репозиторијум
support = Подршка
no-results = Није пронађен ниједан резултат
home = Кућа
open-folder = Отвори директоријум
password = Шифра
networks = Мреже
notification-in-progress = Операције над фајловима су у току.
skip = Прескочи
recents = Скорије
undo = Поништи промену
today = Данас

close = Затвори

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
task-paused = Паузирано
task-paused-waiting = Паузирано, чека
blocked-read = „{$name}“ не може да се прочита
blocked-list = Фасцикла „{$name}“ не може да се отвори
blocked-remove = Ставка „{$name}“ је копирана, али не може да се уклони
same-for-rest = Исто за остале
retry = Покушај поново
retry-as-root = Покушај поново као администратор
use-root-again = Поново користи администраторска права
root-not-granted = Администраторски приступ није одобрен
abort = Заустави
blocked-failed = „{$name}“ није могуће довршити
abort-tooltip = Заустави овде и задржи урађено
cancel-tooltip = Заустави и врати све назад
stalled-title = Уређај не одговара
stalled-for = Без одговора {$seconds} с
keep-original = Задржи оригинал
destination-no-permission = Није могуће писати у „{$folder}“: нема овлашћења
destination-read-only = Није могуће писати у „{$folder}“: само за читање
blocked-link = „{$name}“ је веза, а овај уређај не може да садржи везе
blocked-link-fs = „{$name}“ је веза, а овај уређај ({$fs}) не може да садржи везе
blocked-too-big = Ставка „{$name}“ је превелика за овај уређај ({$fs})
blocked-bad-name = Ставка „{$name}“ садржи знакове које овај уређај ({$fs}) не може да сачува
blocked-delete = Немате дозволу да обришете ставку „{$name}“
blocked-no-trash = Ставка „{$name}“ се налази на диску без отпада
delete-permanently-as-root = Обриши трајно као администратор
deleted-for-good = {$more ->
    [0] Отказано, али је ставка „{$name}“ већ трајно обрисана
    [one] Отказано, али су ставка „{$name}“ и још {$more} ставка већ трајно обрисане
    [few] Отказано, али су ставка „{$name}“ и још {$more} ставке већ трајно обрисане
    *[other] Отказано, али су ставка „{$name}“ и још {$more} ставки већ трајно обрисане
  }
progress-asking = паузирано
failed-path = „{$name}“: {$reason}
reason-no-permission = нема овлашћења
reason-drive-full = уређај је пун
reason-read-only = уређај је само за читање
reason-gone = више не постоји
reason-too-big = превелико је за овај уређај
blocked-move = Ставка „{$name}“ не може да се премести
blocked-move-reason = Оригинали не могу да се уклоне из „{$folder}“: {$reason}
same-for-rest-count = Исто за остале ({$count})
not-enough-space = Потребно је {$needed}, али је слободно само {$free}
in-use = „{$name}“ користи друга операција: {$operation}
checking = Провера… датотеке: {$files}, {$size}
rollback-failed = {$more ->
    [0] Отказано, али ставку „{$name}“ није могуће вратити
    [one] Отказано, али ставку „{$name}“ и још {$more} ставку није могуће вратити
    [few] Отказано, али ставку „{$name}“ и још {$more} ставке није могуће вратити
    *[other] Отказано, али ставку „{$name}“ и још {$more} ставки није могуће вратити
  }
failed-operations-title = {$count ->
    [one] {$count} операција није успела
    [few] {$count} операције нису успеле
    *[other] {$count} операција није успело
  }
merge = Споји
replace-folder-warning = Да ли желите да их спојите или да замените фасциклу која се ту налази? Замена ће је послати у отпад.
folder-totals = { $files ->
    [one] { $files } датотека, { $size }
    [few] { $files } датотеке, { $size }
   *[other] { $files } датотека, { $size }
}
empty-before-eject-title = Испразнити смеће пре избацивања?
empty-before-eject-body = Испразните смеће да бисте ослободили простор на „{$name}“. Све ставке у смећу биће трајно обрисане.
do-not-empty = Не празни
