earth-files = Earth Files
empty-folder = खाली फ़ोल्डर
empty-folder-hidden = खाली फ़ोल्डर (अदृश्य आइटम शामिल हैं)
no-results = कोई परिणाम नहीं
filesystem = फाइल सिस्टम
home = होम
networks = नेटवर्क्स
notification-in-progress = फ़ाइल संचालन प्रगति पर हैं
trash = कचरा
recents = हाल के
search-title = खोज "{$term}": {$name}
undo = पूर्ववत करें
today = आज
# Desktop view options
# List view
name = नाम
modified = संशोधित तिथि
trashed-on = कचरे में डालने की तिथि
size = आकार
type-heading = प्रकार
item-count = {$count ->
        [one] {$count} आइटम
       *[other] {$count} आइटम
    }
file-type-folder = फ़ोल्डर
file-type-image = छवि
file-type-video = वीडियो
file-type-audio = ऑडियो
file-type-text = पाठ
file-type-archive = संग्रह
file-type-document = दस्तावेज़
file-type-program = प्रोग्राम
file-type-other = अन्य

# Dialogs


## Compress Dialog

create-archive = संग्रह बनाएँ

## Empty Trash Dialog

empty-trash = रद्दी साफ़ करें
empty-trash-warning = रद्दी फ़ोल्डर में मौजूद आइटम स्थायी रूप से हटा दिए जाएंगे
empty-before-eject-title = निकालने से पहले रद्दी खाली करें?
empty-before-eject-body = “{$name}” पर जगह खाली करने के लिए रद्दी खाली करें। रद्दी की सभी वस्तुएँ स्थायी रूप से हटा दी जाएँगी।
do-not-empty = खाली न करें

## New File/Folder Dialog

create-new-file = नई फाइल बनाएँ
create-new-folder = नया फ़ोल्डर बनाएँ
file-name = फाइल का नाम
folder-name = फ़ोल्डर का नाम
file-already-exists = उस नाम की फ़ाइल पहले से मौजूद है
folder-already-exists = उस नाम का फ़ोल्डर पहले से मौजूद है
name-hidden = "." से शुरू होने वाले नाम छिपे होंगे
name-invalid = नाम "{ $filename }" नहीं हो सकता
name-no-slashes = नाम में स्लैश नहीं हो सकते

## Open/Save Dialog

cancel = रद्द करें
create = बनाएँ
open = खोलें
open-file = फ़ाइल खोलें
open-folder = फ़ोल्डर खोलें
open-in-new-tab = नई टैब में खोलें
open-in-new-window = नई विंडो में खोलें
open-item-location = आइटम का स्थान खोलें
open-multiple-files = कई फ़ाइलें खोलें
open-multiple-folders = कई फ़ोल्डर खोलें
save = सहेजें
save-file = फ़ाइल सहेजें

## Open With Dialog

open-with-title = "{ $name }" को कैसे खोलना चाहेंगे?
browse-store = { $store } में ब्राउज़ करें

## Rename Dialog

rename-file = फाइल का नाम बदलें
rename-folder = फ़ोल्डर का नाम बदलें

## Replace Dialog

replace = बदलें
replace-title = { $filename } पहले से इस स्थान पर मौजूद है
replace-warning = क्या आप इसे प्रतिस्थापित करना चाहते हैं? यदि प्रतिस्थापित किया गया, तो मौजूदा फ़ाइल को ओवरराइट किया जाएगा।
replace-warning-operation = क्या आप इसे बदलना चाहते हैं? प्रतिस्थापित करने पर मौजूदा फ़ाइल ओवरराइट हो जाएगी।
original-file = मूल फ़ाइल
replace-with = इसके साथ प्रतिस्थापित करें
apply-to-all = सभी पर लागू करें
keep-both = दोनों रखें
merge = मर्ज करें
replace-folder-warning = क्या आप इन्हें मर्ज करना चाहते हैं, या वहाँ मौजूद फ़ोल्डर को बदलना चाहते हैं? बदलने पर वह कचरे में भेज दिया जाएगा।
folder-totals = { $files ->
    [one] { $files } फ़ाइल, { $size }
   *[other] { $files } फ़ाइलें, { $size }
}
skip = छोड़ें

## Set as Executable and Launch Dialog

set-executable-and-launch = निष्पादन योग्य के रूप में सेट करें और लॉन्च करें
set-executable-and-launch-description = क्या आप निष्पादन योग्य के रूप में "{ $name }" सेट करना चाहते हैं और इसे लॉन्च करते हैं?
set-and-launch = सेट करें और लॉन्च करें
launch-desktop-entry = Launch application?
launch-desktop-entry-description = "{$name}" is not an installed application. Launching it runs the command below.
launch-anyway = Launch

## Metadata Dialog

owner = मालिक
group = समूह
other = अन्य

# Context Pages


## About


## Add Network Drive

add-network-drive = नेटवर्क ड्राइव जोड़ें
network-drives-connected = { $count } कनेक्टेड
connect = कनेक्ट करें
connect-anonymously = गुमनाम रूप से कनेक्ट करें
connecting = कनेक्ट हो रहा है...
domain = डोमेन
enter-server-address = सर्वर का पता दर्ज करें
network-drive-description =
    सर्वर पते प्रोटोकॉल उपसर्ग और पते सहित होते हैं।
    उदाहरण: ssh://192.168.0.1, ftp://[2001:db8::1]

### Make sure to keep the comma which separates the columns

network-drive-schemes =
    उपलब्ध प्रोटोकॉल,उपसर्ग
    एप्पलटॉक,afp://
    फ़ाइल ट्रांसफ़र प्रोटोकॉल,ftp:// या ftps://
    नेटवर्क फ़ाइल सिस्टम,nfs://
    सर्वर संदेश ब्लॉक,smb://
    SSH फ़ाइल ट्रांसफ़र प्रोटोकॉल,sftp:// या ssh://
    वेबDAV,dav:// या davs://
network-drive-error = नेटवर्क ड्राइव तक पहुंचने में असमर्थ
password = पासवर्ड
remember-password = पासवर्ड याद रखें
try-again = फिर से कोशिश करें
username = उपयोगकर्ता नाम

## Operations

edit-history = संपादन इतिहास
history = इतिहास
no-history = इतिहास में कोई आइटम नहीं
pending = लंबित
failed = विफल
complete = पूर्ण
compressing =
    संपीड़ित किया जा रहा है { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to }
compressed =
    संपीड़ित किया गया { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to }
copy_noun = नकल
creating = { $parent } में { $name } बनाया जा रहा है
created = { $parent } में { $name } बनाया गया
copying =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to } नकल की जा रही है
copied =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to } नकल की गई
emptying-trash = कचरा खाली किया जा रहा है
emptied-trash = कचरा खाली किया गया
extracting =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to } निकाला जा रहा है
extracted =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to } निकाला गया
setting-executable-and-launching = "{ $name }" को कार्यान्वयन के रूप में सेट किया जा रहा है और लॉन्च किया जा रहा है
set-executable-and-launched = "{ $name }" को कार्यान्वयन के रूप में सेट किया गया है और लॉन्च किया गया है
moving =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to } स्थानांतरित किया जा रहा है
moved =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } से { $from } तक { $to } स्थानांतरित किया गया
renaming = { $from } से { $to } तक नाम बदला जा रहा है
renamed = { $from } से { $to } तक नाम बदला गया
renaming-many = { item-count } का नाम बदला जा रहा है
renamed-many = { item-count } का नाम बदला गया
restoring =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } को कचरे से पुनर्स्थापित किया जा रहा है
restored =
    { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } को कचरे से पुनर्स्थापित किया गया
unknown-folder = अज्ञात फ़ोल्डर

## Open with

menu-open-with = इसके साथ खोलें
default-app = { $name } (डिफ़ॉल्ट)

## Show details

show-details = विवरण दिखाएँ

## Settings

settings = सेटिंग्स

### Appearance

appearance = रुप-रंग
theme = प्रसंग
match-desktop = डेस्कटॉप से मेल खाएँ
dark = डार्क
light = लाइट
# Context menu
add-to-sidebar = साइडबार में जोड़ें
compress = संपीड़ित करें
extract-here = यहाँ निकालें
new-file = नई फ़ाइल...
new-folder = नया फ़ोल्डर...
open-in-terminal = टर्मिनल में खोलें
move-to-trash = कचरे में भेजें
restore-from-trash = कचरे से पुनर्स्थापित करें
trash-original-location = मूल स्थान: {$location}
trash-original-unknown = अज्ञात
remove-from-sidebar = साइडबार से निकालें
removed-from-sidebar = { $name } को साइडबार से निकाला गया

## Desktop


# Menu


## File

file = फ़ाइल
new-tab = नया टैब
new-window = नई विंडो
rename = नाम बदलें...
close-tab = टैब बंद करें
quit = बाहर जाएँ

## Edit

edit = संपादन
cut = काटें
copy = कॉपी करें
paste = चिपकाएँ
select-all = सभी चुनें
image-load-error = ⚠ {$error}
loading-full-image = Loading higher resolution...

## View

zoom-in = बड़ा करें
default-size = मूल आकार
zoom-out = छोटा करें
view = दृश्य
grid-view = ग्रिड दृश्य
list-view = सूची दृश्य
show-hidden-files = छिपी हुई फाइलें दिखाएँ
show-type-column = प्रकार कॉलम दिखाएँ
list-directories-first = सबसे पहले डाइरेक्ट्री दिखाएँ
menu-settings = सेटिंग्स..।
menu-about = { earth-files } के बारे में...

## Sort

sort = क्रमबद्ध करें
sort-a-z = अ-ह क्रम में क्रमबद्ध करें
sort-z-a = ह-अ क्रम में क्रमबद्ध करें
sort-newest-first = नए से पुराने
sort-oldest-first = पुराने से नए
sort-smallest-to-largest = छोटे से बड़े
sort-largest-to-smallest = बड़े से छोटे
sort-type-a-z = प्रकार अ-ह क्रम में क्रमबद्ध करें
sort-type-z-a = प्रकार ह-अ क्रम में क्रमबद्ध करें
repository = रिपॉजिटरी
support = समर्थन
read-execute = पढ़ें और निष्पादित करें
deleted =
    { trash } से { $items } { $items ->
        [one] आइटम मिटाया गया
       *[other] आइटम मिटाए गए
    }
favorite-path-error = निर्देशिका खोलने में त्रुटि
progress = { $percent }%
related-apps = संबंधित ऐप्स
removing-from-recents =
    { recents } से { $items } { $items ->
        [one] आइटम हटाया जा रहा है
       *[other] आइटम हटाए जा रहे हैं
    }
remove = हटाएँ
read-write-execute = पढ़ें, लिखें और निष्पादित करें
other-apps = अन्य ऐप्स
pause = विराम
keep = रखें
permanently-deleting =
    { $items } { $items ->
        [one] आइटम स्थायी रूप से मिटाया जा रहा है
       *[other] आइटम स्थायी रूप से मिटाए जा रहे हैं
    }
read-write = पढ़ें और लिखें
none = कोई नहीं
resume = फिर से शुरू करें
tasks-count = { $count } कार्य
tasks-more = { $count } और
task-loading = { $name } लोड हो रहा है
task-mounting = { $name } माउंट हो रहा है
task-unmounting = { $name } अनमाउंट हो रहा है
task-done = पूर्ण
task-failed = विफल
task-paused = रुका हुआ
task-paused-waiting = रुका हुआ, प्रतीक्षा में
blocked-read = "{$name}" पढ़ा नहीं जा सकता
blocked-list = फ़ोल्डर "{$name}" खोला नहीं जा सकता
blocked-remove = "{$name}" कॉपी हो गया, पर हटाया नहीं जा सकता
same-for-rest = बाकी के लिए भी यही
retry = फिर से कोशिश करें
retry-as-root = व्यवस्थापक के रूप में फिर से कोशिश करें
use-root-again = व्यवस्थापक अधिकार फिर से इस्तेमाल करें
root-not-granted = व्यवस्थापक पहुँच नहीं दी गई
abort = रोकें
blocked-failed = "{$name}" पूरा नहीं हो सका
abort-tooltip = यहीं रोकें और जो हो चुका है उसे रखें
cancel-tooltip = रोकें और सब कुछ पहले जैसा कर दें
stalled-title = ड्राइव जवाब नहीं दे रही है
stalled-for = {$seconds} से. से कोई जवाब नहीं
keep-original = मूल रखें
destination-no-permission = "{$folder}" में लिखा नहीं जा सकता: अनुमति नहीं है
destination-read-only = "{$folder}" में लिखा नहीं जा सकता: यह केवल पढ़ने के लिए है
blocked-link = "{$name}" एक लिंक है, और यह ड्राइव लिंक नहीं रख सकती
blocked-link-fs = "{$name}" एक लिंक है, और यह ड्राइव ({$fs}) लिंक नहीं रख सकती
blocked-too-big = "{$name}" इस ड्राइव ({$fs}) के लिए बहुत बड़ा है
blocked-bad-name = "{$name}" में ऐसे अक्षर हैं जिन्हें यह ड्राइव ({$fs}) नहीं रख सकती
blocked-delete = आपके पास "{$name}" को हटाने की अनुमति नहीं है
blocked-no-trash = "{$name}" ऐसी ड्राइव पर है जिसमें कचरा नहीं है
delete-permanently-as-root = व्यवस्थापक के रूप में स्थायी रूप से हटाएँ
deleted-for-good = {$more ->
    [0] रद्द किया गया, लेकिन "{$name}" पहले ही स्थायी रूप से हटाया जा चुका है
    *[other] रद्द किया गया, लेकिन "{$name}" और {$more} अन्य पहले ही स्थायी रूप से हटाए जा चुके हैं
  }
progress-asking = रुका हुआ
failed-path = "{$name}": {$reason}
reason-no-permission = अनुमति नहीं है
reason-drive-full = ड्राइव भरी हुई है
reason-read-only = ड्राइव केवल पढ़ने के लिए है
reason-gone = यह अब मौजूद नहीं है
reason-too-big = यह इस ड्राइव के लिए बहुत बड़ा है
blocked-move = "{$name}" मूव नहीं किया जा सकता
blocked-move-reason = "{$folder}" से मूल फ़ाइलें हटाई नहीं जा सकतीं: {$reason}
same-for-rest-count = बाकी के लिए भी यही ({$count})
not-enough-space = इसके लिए {$needed} चाहिए, लेकिन केवल {$free} खाली है
in-use = "{$name}" किसी दूसरे ऑपरेशन द्वारा उपयोग में है: {$operation}
checking = जाँच हो रही है… फ़ाइलें: {$files}, {$size}
rollback-failed = {$more ->
    [0] रद्द किया गया, लेकिन "{$name}" को वापस नहीं रखा जा सका
    *[other] रद्द किया गया, लेकिन "{$name}" और {$more} अन्य को वापस नहीं रखा जा सका
  }
failed-operations-title = {$count ->
    [one] कार्रवाई विफल रही
    *[other] {$count} कार्रवाइयाँ विफल रहीं
  }
extract-as-folder = फ़ोल्डर में निकालें
extract-to = इस रूप में निकालें..।
delete = हटाएं
read-only = केवल पढ़ने के लिए
deleting =
    { trash } से { $items } { $items ->
        [one] आइटम मिटाया जा रहा है
       *[other] आइटम मिटाए जा रहे हैं
    } ({ $progress })..।
execute-only = केवल निष्पादित करें
details = विवरण
mount-error = ड्राइव तक पहुँचने में असमर्थ
removed-from-recents =
    { recents } से { $items } { $items ->
        [one] आइटम हटाया गया
       *[other] आइटम हटाए गए
    }
progress-paused = { $percent }%, रुका हुआ
cancelled = रद्द किया गया
operation-failed-to-start = The file operation could not be started
progress-failed = { $percent }%, विफल
extract-to-title = फ़ोल्डर में निकालें
open-with = इससे खोलें
permanently-deleted =
    { $items } { $items ->
        [one] आइटम स्थायी रूप से मिटाया गया
       *[other] आइटम स्थायी रूप से मिटाए गए
    }
write-execute = लिखें और निष्पादित करें
extract-password-required = पासवर्ड आवश्यक है
progress-cancelled = { $percent }%, रद्द किया गया
write-only = केवल लिखने के लिए
permanently-delete-warning = { $target } को स्थाई रूप से हटा दिया जाएगा। इस कार्रवाई को पूर्ववत नहीं किया जा सकता है।
favorite-path-error-description =
    "{ $path }" को खोला नहीं जा सकता
    "{ $path }" मौजूद नहीं हो सकता है या आपके पास इसे खोलने की अनुमति नहीं हो सकती है

    क्या आप इसे साइडबार से हटाना चाहते हैं?
empty-trash-title = रद्दी साफ़ करें?
permanently-delete-question = स्थाई रूप से हटाएं?
copy-to-title = कॉपी गंतव्य चुनें
copy-to-button-label = कॉपी
move-to-title = मूव गंतव्य चुनें
move-to-button-label = मूव
context-action = संदर्भ क्रिया
context-action-confirm-title = "{ $name }" चलाएँ?
context-action-confirm-warning =
    यह { $items } { $items ->
        [one] आइटम
       *[other] आइटम
    } पर चलेगा।
run = चलाएँ
rename-confirm = नाम बदलें

## Batch Rename Dialog
batch-rename-title = {$count} आइटम का नाम बदलें
batch-rename-template = टेम्पलेट
batch-rename-find-replace = खोजें और बदलें
batch-rename-new-name = नया नाम
batch-rename-tag-name = [मूल नाम]
batch-rename-tag-number = [1, 2, 3]
batch-rename-add-name = मूल नाम जोड़ें
batch-rename-add-number = संख्या जोड़ें
find = खोजें
batch-rename-conflicts = {$count ->
    [one] एक नया नाम किसी मौजूदा आइटम या किसी अन्य नए नाम से टकराता है
    *[other] {$count} नए नाम मौजूदा आइटम या एक-दूसरे से टकराते हैं
  }
mixed = मिश्रित
pasted-image = चिपकाई गई छवि
pasted-text = चिपकाया गया पाठ
pasted-video = चिपकाया गया वीडियो

close = बंद करें

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
