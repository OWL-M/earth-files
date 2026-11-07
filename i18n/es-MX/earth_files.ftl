earth-files = Earth Files

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
task-paused = Pausado
task-paused-waiting = Pausado, en espera
blocked-read = No se puede leer "{$name}"
blocked-list = No se puede abrir la carpeta "{$name}"
blocked-remove = "{$name}" se copió, pero no se puede eliminar
same-for-rest = Lo mismo para el resto
retry = Intentar de nuevo
retry-as-root = Intentar de nuevo como administrador
use-root-again = Usar de nuevo los permisos de administrador
root-not-granted = No se concedió el acceso de administrador
abort = Detener
blocked-failed = No se pudo terminar "{$name}"
abort-tooltip = Detener aquí y mantener lo que ya está hecho
cancel-tooltip = Detener y dejar todo como estaba
stalled-title = La unidad no responde
stalled-for = Sin respuesta desde hace {$seconds} s
keep-waiting = Seguir esperando
keep-original = Mantener el original
destination-no-permission = No se puede escribir en "{$folder}": sin permiso
destination-read-only = No se puede escribir en "{$folder}": es de solo lectura
blocked-link = "{$name}" es un enlace, y esta unidad no puede contener enlaces
blocked-link-fs = "{$name}" es un enlace, y esta unidad ({$fs}) no puede contener enlaces
blocked-too-big = "{$name}" es demasiado grande para esta unidad ({$fs})
blocked-bad-name = "{$name}" tiene caracteres que esta unidad ({$fs}) no puede contener
blocked-delete = No tienes permiso para eliminar "{$name}"
blocked-no-trash = "{$name}" está en una unidad sin papelera
blocked-replace-no-trash = "{$name}" no puede ir a la papelera en esta unidad: al reemplazarlo se elimina para siempre
delete-permanently-and-replace = Eliminar definitivamente y reemplazar
delete-permanently-as-root = Eliminar de forma permanente como administrador
deleted-for-good = {$more ->
    [0] Se canceló, pero "{$name}" ya se había eliminado de forma permanente
    *[other] Se canceló, pero "{$name}" y {$more} más ya se habían eliminado de forma permanente
  }
progress-asking = pausado
failed-path = "{$name}": {$reason}
reason-no-permission = sin permiso
reason-drive-full = la unidad está llena
reason-read-only = la unidad es de solo lectura
reason-gone = ya no existe
reason-too-big = es demasiado grande para esta unidad
reason-not-kept-there = la unidad no los conserva
reason-damaged = los datos están dañados
permissions-not-kept-one = No se pudieron conservar los permisos de "{$name}": {$reason}
permissions-not-kept-many = No se pudieron conservar los permisos de {$count} elementos: {$reason}
dates-not-kept-one = No se pudieron conservar las fechas de "{$name}": {$reason}
dates-not-kept-many = No se pudieron conservar las fechas de {$count} elementos: {$reason}
not-extracted-whole-one = No se pudo extraer completo "{$name}": {$reason}
not-extracted-whole-many = No se pudieron extraer completos {$count} elementos: {$reason}
blocked-move = No se puede mover "{$name}"
blocked-move-reason = No se pueden eliminar los originales de "{$folder}": {$reason}
same-for-rest-count = Lo mismo para el resto ({$count})
not-enough-space = Se necesitan {$needed}, pero solo hay {$free} libres
in-use = "{$name}" está en uso por otra operación: {$operation}
checking = Verificando… archivos: {$files}, {$size}
transfer-speed = {$size}/s
files-transferred = {$done} de {$total} archivos transferidos
rollback-failed = {$more ->
    [0] Se canceló, pero no se pudo restaurar "{$name}"
    *[other] Se canceló, pero no se pudieron restaurar "{$name}" y {$more} más
  }
failed-operations-title = {$count ->
    [one] La operación falló
    *[other] {$count} operaciones fallaron
  }
merge = Combinar
replace-folder-warning = ¿Quieres combinarlas o reemplazar la carpeta que hay allí? Al reemplazarla se enviará a la papelera.
folder-totals = { $files ->
    [one] { $files } archivo, { $size }
   *[other] { $files } archivos, { $size }
}
trash-original-location = Ubicación original: {$location}
trash-original-unknown = Desconocida
empty-before-eject-title = ¿Vaciar la papelera antes de expulsar?
empty-before-eject-body = Vacía la papelera para liberar espacio en “{$name}”. Todos los elementos de la papelera se eliminarán de forma permanente.
do-not-empty = No vaciar
