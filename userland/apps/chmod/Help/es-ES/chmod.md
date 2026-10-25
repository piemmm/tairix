## NAME

chmod — cambiar los bits de modo de un archivo

## SYNOPSIS

`chmod [-cfRv] [--] MODE file...`

## DESCRIPTION

Cambia los bits de permiso de cada operando de archivo a `MODE`, en
orden. `MODE` es o bien un valor octal absoluto (`644`, `0755`, …)
que sustituye por completo los bits de permiso, o bien una lista de
cláusulas simbólicas separadas por comas
`[ugoa]*([-+=]([rwxXst]*|[ugo]))+` o `[-+=][0-7]+` (`g+w`, `o-rx`,
`a=rx`, `u+s`, `g=u`, `=644`) que transforman los bits actuales del
archivo. La `X` simbólica concede ejecución solo a un directorio o a
un archivo que ya lleve un bit de ejecución, `g=u` da al grupo los bits
del propietario, `s` activa los bits set-ID del propietario y del
grupo, y `t` activa el bit sticky. Una cláusula que no nombra a nadie
(`+x`, `=r`) cambia todos los campos salvo los bits que contiene la
umask.

En un directorio, los bits set-user-ID y set-group-ID se quedan como
están salvo que el modo los nombre: con `s` (`g-s`), con octal tras un
operador (`=755`) o con un valor octal de cinco o más dígitos
(`00755`). El bit set-group-ID de un directorio es lo que da su grupo a
sus entradas nuevas, así que `chmod 755` lo conserva.

Solo el propietario de un archivo puede cambiar su modo; el núcleo
rechaza a cualquier otro, y poseer una capability no concede ningún
privilegio. Un enlace simbólico nombrado como operando cambia el
archivo que nombra. Con `-R` un operando de directorio se cambia y
después su contenido se cambia recursivamente; un enlace simbólico
encontrado dentro del árbol se deja intacto, y también lo que nombra.
Un archivo que no puede alcanzarse o cambiarse se notifica y los
demás se cambian de todos modos. `--` termina el análisis de
opciones: cada argumento posterior es un operando. Para un modo que
empiece por `-`, escríbalo sin el guion (`a-w`) o termine antes las
opciones (`chmod -- -w file`).

## OPTIONS

- `-R, --recursive` — cambiar archivos y directorios
  recursivamente.
- `-c, --changes` — informar solo de los archivos cuyo modo cambió
  realmente.
- `-v, --verbose` — informar de cada archivo procesado, incluido
  aquel cuyo cambio se rechazó o que no pudo alcanzarse.
- `-f, --silent, --quiet` — suprimir la mayoría de los mensajes de
  error; la ejecución sigue fallando y el estado de salida lo
  refleja.
- `--preserve-root` — con `-R`, negarse a cambiar `/`, se llegue a él
  como se llegue.
- `--no-preserve-root` — tratar `/` como cualquier otro directorio (el
  comportamiento predeterminado).
- `-h, -?, --help` — mostrar la ayuda corta de esta orden.

## EXAMPLES

- `chmod 644 notes.txt` — lectura/escritura para el propietario,
  solo lectura para los demás.
- `chmod g+w shared.txt` — añadir escritura de grupo a los bits
  actuales.
- `chmod -R a=rx Docs` — hacer el árbol `Docs` legible y
  atravesable por todos.
- `chmod +x build.sh` — hacer un guion ejecutable para todos los que
  permita la umask.

## EXIT STATUS

- `0` — todos los cambios de modo tuvieron éxito.
- `1` — un archivo no pudo alcanzarse o cambiarse, o la salida
  falló; cada razón se imprime en la salida de error (suprimida bajo
  `-f`). Una línea de órdenes no entendida, o cuyo operando de modo
  no era ni octal ni simbólico, también termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `ls`
- `mkdir`
- `rm`
