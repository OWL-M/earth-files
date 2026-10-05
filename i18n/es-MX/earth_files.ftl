earth-files = Earth Files

opening-files = Opening…
into-itself = A folder cannot be moved or copied into itself
task-paused = Pausado
task-paused-waiting = Pausado, en espera
task-skipped = Saltados: {$count}
blocked-read = No se puede leer "{$name}"
blocked-list = No se puede abrir la carpeta "{$name}"
blocked-remove = "{$name}" se copió, pero no se puede eliminar
same-for-rest = Lo mismo para el resto
retry = Intentar de nuevo
retry-as-root = Intentar de nuevo como administrador
use-root-again = Usar de nuevo los permisos de administrador
root-not-granted = No se concedió el acceso de administrador
keep-original = Mantener el original
skipped-more = y {$count} más
destination-no-permission = No se puede escribir en "{$folder}": sin permiso
destination-read-only = No se puede escribir en "{$folder}": es de solo lectura
blocked-link = "{$name}" es un enlace, y esta unidad no puede contener enlaces
blocked-link-fs = "{$name}" es un enlace, y esta unidad ({$fs}) no puede contener enlaces
blocked-too-big = "{$name}" es demasiado grande para esta unidad ({$fs})
blocked-bad-name = "{$name}" tiene caracteres que esta unidad ({$fs}) no puede contener
progress-asking = pausado
failed-path = "{$name}": {$reason}
reason-no-permission = sin permiso
reason-drive-full = la unidad está llena
reason-read-only = la unidad es de solo lectura
reason-gone = ya no existe
reason-too-big = es demasiado grande para esta unidad
blocked-move = No se puede mover "{$name}"
blocked-move-reason = No se pueden eliminar los originales de "{$folder}": {$reason}
same-for-rest-count = Lo mismo para el resto ({$count})
not-enough-space = Se necesitan {$needed}, pero solo hay {$free} libres
checking = Verificando… archivos: {$files}, {$size}
rollback-failed = {$more ->
    [0] Se canceló, pero no se pudo restaurar "{$name}"
    *[other] Se canceló, pero no se pudieron restaurar "{$name}" y {$more} más
  }
failed-operations-title = {$count ->
    [one] La operación falló
    *[other] {$count} operaciones fallaron
  }
