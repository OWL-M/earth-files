earth-files = Earth Files
empty-folder = Dosar gol
empty-folder-hidden = Dosar gol (conține elemente ascunse)
no-results = Niciun rezultat găsit
filesystem = Sistem de fișiere
home = Acasă
networks = Rețele
notification-in-progress = Operațiuni de fișiere în desfășurare.
trash = Coș de gunoi
recents = Recente
search-title = Căutare "{$term}": {$name}
undo = Anulează
today = Astăzi
# Desktop view options
desktop-view-options = Opțiuni de vizualizare desktop...
show-on-desktop = Afișează pe desktop
desktop-folder-content = Conținut dosar desktop
mounted-drives = Unități montate
trash-folder-icon = Pictogramă coș de gunoi
icon-size-and-spacing = Dimensiune și spațiere pictograme
icon-size = Dimensiune pictogramă
grid-spacing = Spațiere grilă
# List view
name = Nume
modified = Modificat
trashed-on = Șters
size = Dimensiune
type-heading = Tip
file-type-folder = Dosar
file-type-image = Imagine
file-type-video = Video
file-type-audio = Audio
file-type-text = Text
file-type-archive = Arhivă
file-type-document = Document
file-type-program = Program
file-type-other = Altele
# Progress footer
details = Detalii
dismiss = Închide mesajul
operations-running = { $running } operațiuni în desfășurare ({ $percent }%)...
operations-running-finished = { $running } operațiuni în desfășurare ({ $percent }%), { $finished } finalizate...
pause = Pauză
resume = Reia

# Dialogs


## Compress Dialog

create-archive = Creează o arhivă

## Extract Dialog

extract-password-required = Parolă necesară
extract-as-folder = Extrage în dosar
extract-to = Extrage în...
extract-to-title = Extrage în dosar

## Empty Trash Dialog

empty-trash = Golește coșul
empty-trash-warning = Sigur dorești să ștergi definitiv toate elementele din coș?

## Dialog Eroare Montare

mount-error = Nu se poate accesa unitatea

## New File/Folder Dialog

create-new-file = Creează un fișier nou
create-new-folder = Creează un dosar nou
file-name = Nume fișier
folder-name = Nume dosar
file-already-exists = Un fișier cu acest nume există deja.
folder-already-exists = Un dosar cu acest nume există deja.
name-hidden = Numele care încep cu „.” vor fi ascunse.
name-invalid = Numele nu poate fi "{ $filename }".
name-no-slashes = Numele nu poate conține caractere „/”.

## Open/Save Dialog

cancel = Anulează
create = Creează
open = Deschide
open-file = Deschide fișier
open-folder = Deschide dosar
open-in-new-tab = Deschide în filă nouă
open-in-new-window = Deschide în fereastră nouă
open-item-location = Deschide locația elementului
open-multiple-files = Deschide fișiere multiple
open-multiple-folders = Deschide dosare multiple
save = Salvează
save-file = Salvează fișier

## Open With Dialog

open-with-title = Cum dorești să deschizi „{ $name }”?
browse-store = Răsfoiește în { $store }

## Rename Dialog

rename-file = Redenumește fișier
rename-folder = Redenumește dosar

## Batch Rename Dialog
batch-rename-title = Redenumește {$count} elemente
batch-rename-template = Șablon
batch-rename-find-replace = Caută și înlocuiește
batch-rename-new-name = Nume nou
batch-rename-tag-name = [Numele original]
batch-rename-tag-number = [1, 2, 3]
batch-rename-add-name = Adaugă numele original
batch-rename-add-number = Adaugă un număr
find = Caută
batch-rename-conflicts = {$count ->
    [one] Un nume nou intră în conflict cu un element existent sau cu alt nume nou
    [few] {$count} nume noi intră în conflict cu elemente existente sau între ele
    *[other] {$count} de nume noi intră în conflict cu elemente existente sau între ele
  }

## Replace Dialog

replace = Înlocuiește
replace-title = „{ $filename }” există deja în această locație.
replace-warning = Dorești să-l înlocuiești cu cel pe care îl salvezi? Această acțiune va suprascrie conținutul.
replace-warning-operation = Dorești să-l înlocuiești? Această acțiune va suprascrie conținutul.
original-file = Fișier original
replace-with = Înlocuiește cu
apply-to-all = Aplică la toate
keep-both = Păstrează ambele
skip = Omitere

## Set as Executable and Launch Dialog

set-executable-and-launch = Fă executabil și rulează
set-executable-and-launch-description = Dorești să setezi „{ $name }” ca executabil și să îl rulezi?
set-and-launch = Setează și rulează
launch-desktop-entry = Launch application?
launch-desktop-entry-description = "{$name}" is not an installed application. Launching it runs the command below.
launch-anyway = Launch

## Metadata Dialog

open-with = Deschide cu
owner = Proprietar
group = Grup
other = Altele

### Mode 0

none = Niciuna

### Mode 1 (unusual)

execute-only = Doar executare

### Mode 2 (unusual)

write-only = Doar scriere

### Mode 3 (unusual)

write-execute = Scriere și executare

### Mode 4

read-only = Doar citire

### Mode 5

read-execute = Citire și executare

### Mode 6

read-write = Citire și scriere

### Mode 7

read-write-execute = Citire, scriere și executare

## Favorite Path Error Dialog

favorite-path-error = Eroare la deschiderea directorului
favorite-path-error-description =
    Nu s-a putut deschide „{ $path }”.
    Este posibil să nu existe sau să nu ai permisiuni de acces.

    Vrei să-l elimini din bara laterală?
remove = Elimină
keep = Păstrează

# Context Pages


## About


## Add Network Drive

add-network-drive = Adaugă o unitate de rețea
connect = Conectează
connect-anonymously = Conectare anonimă
connecting = Se conectează...
domain = Domeniu
enter-server-address = Introdu adresa serverului
network-drive-description =
    Adresele serverului includ prefixul protocolului și adresa.
    Exemple: ssh://192.168.0.1, ftp://[2001:db8::1]

### Make sure to keep the comma which separates the columns

network-drive-schemes =
    Protocoale disponibile,Prefix
    AppleTalk,afp://
    File Transfer Protocol,ftp:// sau ftps://
    Network File System,nfs://
    Server Message Block,smb://
    SSH File Transfer Protocol,sftp:// sau ssh://
    WebDav,dav:// sau davs://
network-drive-error = Nu se poate accesa unitatea de rețea
password = Parolă
remember-password = Ține minte parola
try-again = Încearcă din nou
username = Nume utilizator

## Operations

cancelled = Anulat
operation-failed-to-start = The file operation could not be started
edit-history = Editează istoricul
history = Istoric
no-history = Nicio intrare în istoric.
pending = În așteptare
progress = { $percent }%
progress-cancelled = { $percent }%, anulat
progress-paused = { $percent }%, întrerupt
failed = Eșuat
complete = Complet
compressing =
    Se comprimă { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }” ({ $progress })...
compressed =
    Comprimat { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }”
copy_noun = Copiere
creating = Se creează „{ $name }” în „{ $parent }”
created = S-a creat „{ $name }” în „{ $parent }”
copying =
    Se copiază { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }” ({ $progress })...
copied =
    Copiat { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }”
deleting =
    Se șterge { $items } { $items ->
        [one] element
       *[other] elemente
    } din { trash } ({ $progress })...
deleted =
    Șters { $items } { $items ->
        [one] element
       *[other] elemente
    } din { trash }
emptying-trash = Se golește { trash } ({ $progress })...
emptied-trash = Coșul { trash } a fost golit
extracting =
    Se extrage { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }” ({ $progress })...
extracted =
    Extras { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }”
setting-executable-and-launching = Se setează „{ $name }” ca executabil și se rulează
set-executable-and-launched = „{ $name }” setat ca executabil și rulat
moving =
    Se mută { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }” ({ $progress })...
moved =
    Mutat { $items } { $items ->
        [one] element
       *[other] elemente
    } din „{ $from }” în „{ $to }”
renaming = Se redenumește „{ $from }” în „{ $to }”
renamed = S-a redenumit „{ $from }” în „{ $to }”
renaming-many = Se redenumesc { item-count }
renamed-many = S-au redenumit { item-count }
restoring =
    Se restabilește { $items } { $items ->
        [one] element
       *[other] elemente
    } din { trash } ({ $progress })...
restored =
    Restabilit { $items } { $items ->
        [one] element
       *[other] elemente
    } din { trash }
unknown-folder = dosar necunoscut

## Open with

menu-open-with = Deschide cu...
default-app = { $name } (implicit)

## Show details

show-details = Afișează detalii
type = Tip: { $mime }
items = Elemente: { $items }
item-count = {$count ->
        [one] {$count} element
        [few] {$count} elemente
       *[other] {$count} de elemente
    }
item-size = Dimensiune: { $size }
item-created = Creat: { $created }
item-modified = Modificat: { $modified }
item-accessed = Accesat: { $accessed }
calculating = Se calculează...

## Settings

settings = Setări
single-click = Un singur clic pentru deschidere

### Appearance

appearance = Aspect
theme = Temă
match-desktop = Potrivește cu desktopul
dark = Întunecat
light = Deschis

### Type to Search

type-to-search = Tastează pentru a căuta
type-to-search-recursive = Caută în dosarul curent și subdosare
type-to-search-enter-path = Introduce calea către dosar sau fișier
# Context menu
add-to-sidebar = Adaugă în bara laterală
compress = Comprimă
delete-permanently = Șterge definitiv
extract-here = Extrage aici
new-file = Fișier nou...
new-folder = Dosar nou...
open-in-terminal = Deschide în terminal
move-to-trash = Mută în coș
restore-from-trash = Recuperează din coș
remove-from-sidebar = Elimină din bara laterală
sort-by-name = Sortează după nume
sort-by-modified = Sortează după modificare
sort-by-size = Sortează după dimensiune
sort-by-trashed = Sortează după dată ștergere

## Desktop

change-wallpaper = Schimbă fundalul...
desktop-appearance = Aspect desktop...
display-settings = Setări ecran...

# Menu


## File

file = Fișier
new-tab = Filă nouă
new-window = Fereastră nouă
rename = Redenumește...
close-tab = Închide fila
quit = Închide aplicația

## Edit

edit = Editare
cut = Taie
copy = Copiază
paste = Lipește
select-all = Selectează tot
image-load-error = ⚠ {$error}
loading-full-image = Loading higher resolution...

## View

zoom-in = Mărește
default-size = Dimensiune implicită
zoom-out = Micșorează
view = Vizualizare
grid-view = Vizualizare grilă
list-view = Vizualizare listă
show-hidden-files = Afișează fișiere ascunse
show-type-column = Afișează coloana tip
list-directories-first = Listează directoarele primele
gallery-preview = Previzualizare galerie
menu-settings = Setări...
menu-about = Despre { earth-files }...

## Sort

sort = Sortare
sort-a-z = A-Z
sort-z-a = Z-A
sort-newest-first = Cele mai noi primele
sort-oldest-first = Cele mai vechi primele
sort-smallest-to-largest = De la mic la mare
sort-largest-to-smallest = De la mare la mic
sort-type-a-z = Tip A-Z
sort-type-z-a = Tip Z-A

close = Închide
