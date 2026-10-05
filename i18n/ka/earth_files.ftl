earth-files = Earth Files

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
task-paused = შეჩერებულია
task-paused-waiting = შეჩერებულია, ელოდება
blocked-read = „{$name}“-ის წაკითხვა ვერ ხერხდება
blocked-list = საქაღალდის „{$name}“ გახსნა ვერ ხერხდება
blocked-remove = „{$name}“ დაკოპირდა, მაგრამ მისი წაშლა ვერ ხერხდება
same-for-rest = იგივე დანარჩენებისთვის
retry = ხელახლა ცდა
retry-as-root = ხელახლა ცდა ადმინისტრატორად
use-root-again = ადმინისტრატორის უფლებების ხელახლა გამოყენება
root-not-granted = ადმინისტრატორის წვდომა არ მიეცა
abort = შეწყვეტა
blocked-failed = „{$name}“-ის დასრულება ვერ მოხერხდა
abort-tooltip = აქ შეჩერება და შესრულებულის შენარჩუნება
cancel-tooltip = შეჩერება და ყველაფრის დაბრუნება
stalled-title = დისკი არ პასუხობს
stalled-for = პასუხი არ არის {$seconds} წმ
keep-original = ორიგინალის შენარჩუნება
destination-no-permission = „{$folder}“-ში ჩაწერა ვერ ხერხდება: წვდომა არ არის
destination-read-only = „{$folder}“-ში ჩაწერა ვერ ხერხდება: მხოლოდ წასაკითხია
blocked-link = „{$name}“ ბმულია და ამ დისკს ბმულების შენახვა არ შეუძლია
blocked-link-fs = „{$name}“ ბმულია და ამ დისკს ({$fs}) ბმულების შენახვა არ შეუძლია
blocked-too-big = „{$name}“ ძალიან დიდია ამ დისკისთვის ({$fs})
blocked-bad-name = „{$name}“ შეიცავს სიმბოლოებს, რომელთა შენახვაც ამ დისკს ({$fs}) არ შეუძლია
blocked-delete = „{$name}“-ის წასაშლელად ნებართვა არ გაქვთ
blocked-no-trash = „{$name}“ იმყოფება დისკზე, რომელსაც ნაგავი არ აქვს
blocked-trash-full = ნაგავში „{$name}“-ისთვის ადგილი არ არის
delete-permanently-as-root = სამუდამოდ წაშლა ადმინისტრატორად
deleted-for-good = {$more ->
    [0] გაუქმდა, მაგრამ „{$name}“ უკვე სამუდამოდ წაიშალა
    *[other] გაუქმდა, მაგრამ „{$name}“ და კიდევ {$more} ელემენტი უკვე სამუდამოდ წაიშალა
  }
progress-asking = შეჩერებულია
failed-path = „{$name}“: {$reason}
reason-no-permission = წვდომა არ არის
reason-drive-full = დისკი სავსეა
reason-read-only = დისკი მხოლოდ წასაკითხია
reason-gone = აღარ არსებობს
reason-too-big = ამ დისკისთვის ზედმეტად დიდია
blocked-move = „{$name}“-ის გადატანა ვერ ხერხდება
blocked-move-reason = „{$folder}“-დან ორიგინალების წაშლა ვერ ხერხდება: {$reason}
same-for-rest-count = იგივე დანარჩენებისთვის ({$count})
not-enough-space = საჭიროა {$needed}, მაგრამ თავისუფალია მხოლოდ {$free}
in-use = „{$name}“ გამოიყენება სხვა ოპერაციის მიერ: {$operation}
checking = მოწმდება… ფაილები: {$files}, {$size}
rollback-failed = {$more ->
    [0] გაუქმდა, მაგრამ „{$name}“-ის დაბრუნება ვერ მოხერხდა
    *[other] გაუქმდა, მაგრამ „{$name}“-ის და კიდევ {$more} ელემენტის დაბრუნება ვერ მოხერხდა
  }
failed-operations-title = {$count ->
    [one] ოპერაცია ვერ შესრულდა
    *[other] {$count} ოპერაცია ვერ შესრულდა
  }
merge = შერწყმა
replace-folder-warning = გსურთ მათი შერწყმა თუ იქ არსებული საქაღალდის ჩანაცვლება? ჩანაცვლებისას ის ნაგვის ყუთში გადავა.
folder-totals = { $files ->
    [one] { $files } ფაილი, { $size }
   *[other] { $files } ფაილი, { $size }
}
