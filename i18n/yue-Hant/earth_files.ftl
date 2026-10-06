earth-files = Earth Files

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
task-paused = 已暫停
task-paused-waiting = 已暫停，等緊
blocked-read = 讀唔到「{$name}」
blocked-list = 開唔到資料夾「{$name}」
blocked-remove = 「{$name}」已經複製咗，但係刪唔到
same-for-rest = 其餘項目都照樣處理
retry = 重試
retry-as-root = 以管理員身分重試
use-root-again = 再用管理員權限
root-not-granted = 未獲授予管理員權限
abort = 中止
blocked-failed = 完成唔到「{$name}」
abort-tooltip = 喺度停，保留已經做咗嘅部分
cancel-tooltip = 停低，將所有嘢還原
stalled-title = 磁碟機冇反應
stalled-for = {$seconds} 秒冇反應
keep-original = 保留原檔
destination-no-permission = 寫唔入「{$folder}」：冇權限
destination-read-only = 寫唔入「{$folder}」：唯讀
blocked-link = 「{$name}」係連結，呢個磁碟機存唔到連結
blocked-link-fs = 「{$name}」係連結，呢個磁碟機（{$fs}）存唔到連結
blocked-too-big = 「{$name}」太大，呢個磁碟機（{$fs}）放唔落
blocked-bad-name = 「{$name}」有啲字元呢個磁碟機（{$fs}）存唔到
blocked-delete = 你冇權限刪除「{$name}」
blocked-no-trash = 「{$name}」喺冇垃圾桶嘅磁碟機上
delete-permanently-as-root = 以管理員身分永久刪除
deleted-for-good = {$more ->
    [0] 已經取消，但係「{$name}」早已永久刪除咗
    *[other] 已經取消，但係「{$name}」同另外 {$more} 個項目早已永久刪除咗
  }
progress-asking = 已暫停
failed-path = 「{$name}」：{$reason}
reason-no-permission = 冇權限
reason-drive-full = 磁碟機滿咗
reason-read-only = 磁碟機係唯讀
reason-gone = 已經唔存在
reason-too-big = 對呢個磁碟機嚟講太大
blocked-move = 搬唔到「{$name}」
blocked-move-reason = 喺「{$folder}」刪除唔到原本嘅檔案：{$reason}
same-for-rest-count = 其餘項目都照樣處理 ({$count})
not-enough-space = 需要 {$needed}，但係只係得 {$free} 可用
in-use = 「{$name}」正被另一個操作使用：{$operation}
checking = 檢查緊… {$files} 個檔案，{$size}
rollback-failed = {$more ->
    [0] 已經取消，但係「{$name}」放唔返原位
    *[other] 已經取消，但係「{$name}」同另外 {$more} 個項目放唔返原位
  }
failed-operations-title = {$count ->
    *[other] {$count} 項操作失敗咗
  }
merge = 合併
replace-folder-warning = 你想合併佢哋，定係取代嗰度已有嘅資料夾？取代會將佢移去垃圾桶。
folder-totals = { $files ->
   *[other] { $files } 個檔案，{ $size }
}
trash-original-location = 原本位置：{$location}
trash-original-unknown = 不明
empty-before-eject-title = 退出之前清空垃圾桶？
empty-before-eject-body = 清空垃圾桶以騰出「{$name}」上嘅空間。垃圾桶入面所有項目都會被永久刪除。
do-not-empty = 唔好清空
