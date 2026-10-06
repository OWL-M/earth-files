earth-files = Earth Files
empty-folder = Prazna fascikla
empty-folder-hidden = Prazna fascikla (ima skrivene stavke)
filesystem = Sistem datoteka
trash = Otpad

# Context Pages


## Properties


## Settings

settings = Podešavanja

### Appearance

appearance = Izgled
theme = Tema
match-desktop = Kao sistem
dark = Tamna
light = Svetla
# Context menu
new-file = Nova datoteka
new-folder = Nova fascikla
move-to-trash = Premesti u otpad
restore-from-trash = Vrati iz otpada
trash-original-location = Izvorna lokacija: {$location}
trash-original-unknown = Nepoznato

# Menu


## File

file = Datoteka
new-tab = Nova kartica
new-window = Novi prozor
close-tab = Zatvori karticu
quit = Izađi

## Edit

edit = Uredi
cut = Iseci
copy = Kopiraj
paste = Nalepi
select-all = Izaberi sve
image-load-error = ⚠ {$error}
loading-full-image = Loading higher resolution...

## View

view = Prikaz
grid-view = Prikaži mrežu
list-view = Prikaži spisak
menu-settings = Podešavanja...
repository = Repozitorijum
support = Podrška
cancel = Poništi
zoom-in = Uvećaj
default-size = Podrazumevana veličina
zoom-out = Umanji

close = Zatvori

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
task-paused = Pauzirano
task-paused-waiting = Pauzirano, čeka
blocked-read = „{$name}“ ne može da se pročita
blocked-list = Fascikla „{$name}“ ne može da se otvori
blocked-remove = Stavka „{$name}“ je kopirana, ali ne može da se ukloni
same-for-rest = Isto za ostale
retry = Pokušaj ponovo
retry-as-root = Pokušaj ponovo kao administrator
use-root-again = Ponovo koristi administratorska prava
root-not-granted = Administratorski pristup nije odobren
abort = Zaustavi
blocked-failed = „{$name}“ nije moguće dovršiti
abort-tooltip = Zaustavi ovde i zadrži urađeno
cancel-tooltip = Zaustavi i vrati sve nazad
stalled-title = Uređaj ne odgovara
stalled-for = Bez odgovora {$seconds} s
keep-original = Zadrži original
destination-no-permission = Nije moguće pisati u „{$folder}“: nema ovlašćenja
destination-read-only = Nije moguće pisati u „{$folder}“: samo za čitanje
blocked-link = „{$name}“ je veza, a ovaj uređaj ne može da sadrži veze
blocked-link-fs = „{$name}“ je veza, a ovaj uređaj ({$fs}) ne može da sadrži veze
blocked-too-big = Stavka „{$name}“ je prevelika za ovaj uređaj ({$fs})
blocked-bad-name = Stavka „{$name}“ sadrži znakove koje ovaj uređaj ({$fs}) ne može da sačuva
blocked-delete = Nemate dozvolu da obrišete stavku „{$name}“
blocked-no-trash = Stavka „{$name}“ se nalazi na disku bez otpada
delete-permanently-as-root = Obriši trajno kao administrator
deleted-for-good = {$more ->
    [0] Otkazano, ali je stavka „{$name}“ već trajno obrisana
    [one] Otkazano, ali su stavka „{$name}“ i još {$more} stavka već trajno obrisane
    [few] Otkazano, ali su stavka „{$name}“ i još {$more} stavke već trajno obrisane
    *[other] Otkazano, ali su stavka „{$name}“ i još {$more} stavki već trajno obrisane
  }
progress-asking = pauzirano
failed-path = „{$name}“: {$reason}
reason-no-permission = nema ovlašćenja
reason-drive-full = uređaj je pun
reason-read-only = uređaj je samo za čitanje
reason-gone = više ne postoji
reason-too-big = preveliko je za ovaj uređaj
blocked-move = Stavka „{$name}“ ne može da se premesti
blocked-move-reason = Originali ne mogu da se uklone iz „{$folder}“: {$reason}
same-for-rest-count = Isto za ostale ({$count})
not-enough-space = Potrebno je {$needed}, ali je slobodno samo {$free}
in-use = „{$name}“ koristi druga operacija: {$operation}
checking = Provera… datoteke: {$files}, {$size}
rollback-failed = {$more ->
    [0] Otkazano, ali stavku „{$name}“ nije moguće vratiti
    [one] Otkazano, ali stavku „{$name}“ i još {$more} stavku nije moguće vratiti
    [few] Otkazano, ali stavku „{$name}“ i još {$more} stavke nije moguće vratiti
    *[other] Otkazano, ali stavku „{$name}“ i još {$more} stavki nije moguće vratiti
  }
failed-operations-title = {$count ->
    [one] {$count} operacija nije uspela
    [few] {$count} operacije nisu uspele
    *[other] {$count} operacija nije uspelo
  }
merge = Spoji
replace-folder-warning = Da li želite da ih spojite ili da zamenite fasciklu koja se tu nalazi? Zamena će je poslati u otpad.
folder-totals = { $files ->
    [one] { $files } datoteka, { $size }
    [few] { $files } datoteke, { $size }
   *[other] { $files } datoteka, { $size }
}
empty-before-eject-title = Isprazniti smeće pre izbacivanja?
empty-before-eject-body = Ispraznite smeće da biste oslobodili prostor na „{$name}“. Sve stavke u smeću biće trajno obrisane.
do-not-empty = Ne prazni
