earth-files = Earth Files

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
task-paused = বিরতিতে
task-paused-waiting = বিরতিতে, অপেক্ষমাণ
blocked-read = "{$name}" পড়া যাচ্ছে না
blocked-list = "{$name}" ফোল্ডারটি খোলা যাচ্ছে না
blocked-remove = "{$name}" কপি করা হয়েছে কিন্তু সরানো যাচ্ছে না
same-for-rest = বাকিগুলোর জন্যও একই
retry = আবার চেষ্টা করুন
retry-as-root = প্রশাসক হিসেবে আবার চেষ্টা করুন
use-root-again = আবার প্রশাসকের অধিকার ব্যবহার করুন
root-not-granted = প্রশাসকের অ্যাক্সেস দেওয়া হয়নি
abort = থামান
blocked-failed = "{$name}" শেষ করা যায়নি
abort-tooltip = এখানে থামুন এবং যা হয়েছে তা রাখুন
cancel-tooltip = থামুন এবং সবকিছু আগের মতো ফিরিয়ে দিন
stalled-title = ড্রাইভটি সাড়া দিচ্ছে না
stalled-for = {$seconds} সে. ধরে কোনো সাড়া নেই
keep-original = মূলটি রাখুন
destination-no-permission = "{$folder}"-এ লেখা যাচ্ছে না: অনুমতি নেই
destination-read-only = "{$folder}"-এ লেখা যাচ্ছে না: এটি শুধু-পঠনযোগ্য
blocked-link = "{$name}" একটি লিংক, এবং এই ড্রাইভে লিংক রাখা যায় না
blocked-link-fs = "{$name}" একটি লিংক, এবং এই ড্রাইভে ({$fs}) লিংক রাখা যায় না
blocked-too-big = "{$name}" এই ড্রাইভের ({$fs}) জন্য খুব বড়
blocked-bad-name = "{$name}"-এ এমন অক্ষর আছে যা এই ড্রাইভে ({$fs}) রাখা যায় না
progress-asking = বিরতিতে
failed-path = "{$name}": {$reason}
reason-no-permission = অনুমতি নেই
reason-drive-full = ড্রাইভটি পূর্ণ
reason-read-only = ড্রাইভটি শুধু-পঠনযোগ্য
reason-gone = এটি আর নেই
reason-too-big = এটি এই ড্রাইভের জন্য অনেক বড়
blocked-move = "{$name}" স্থানান্তর করা যাচ্ছে না
blocked-move-reason = "{$folder}" থেকে মূল ফাইলগুলো সরানো যাচ্ছে না: {$reason}
same-for-rest-count = বাকিগুলোর জন্যও একই ({$count})
not-enough-space = এর জন্য {$needed} প্রয়োজন, কিন্তু মাত্র {$free} খালি আছে
checking = যাচাই করা হচ্ছে… ফাইল: {$files}, {$size}
rollback-failed = {$more ->
    [0] বাতিল করা হয়েছে, কিন্তু "{$name}" আগের জায়গায় ফেরানো যাচ্ছে না
    *[other] বাতিল করা হয়েছে, কিন্তু "{$name}" এবং আরও {$more}টি আগের জায়গায় ফেরানো যাচ্ছে না
  }
failed-operations-title = {$count ->
    [one] অপারেশনটি ব্যর্থ হয়েছে
    *[other] {$count}টি অপারেশন ব্যর্থ হয়েছে
  }
merge = মার্জ করুন
replace-folder-warning = আপনি কি এগুলো মার্জ করতে চান, নাকি সেখানে থাকা ফোল্ডারটি প্রতিস্থাপন করতে চান? প্রতিস্থাপন করলে সেটি ট্র্যাশে পাঠানো হবে।
folder-totals = { $files ->
    [one] { $files }টি ফাইল, { $size }
   *[other] { $files }টি ফাইল, { $size }
}
