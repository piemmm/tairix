## NAME

chown — cambiar el propietario y el grupo de un archivo

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

Cambia el usuario y el grupo propietarios de cada operando de archivo,
en orden. `OWNER` cambia el usuario y conserva el grupo, `OWNER:GROUP`
cambia ambos y `:GROUP` cambia solo el grupo. `OWNER:` además da al
archivo el grupo de inicio de sesión del propietario. Un operando vacío,
o `:` solo, no cambia ninguno de los dos. La forma antigua `OWNER.GROUP`
funciona con un aviso, salvo que el operando entero nombre una cuenta.
Con `--reference`, cada archivo toma el propietario y el grupo de
aquello que nombra `RFILE`.

`OWNER` y `GROUP` son nombres de cuenta y de grupo, o números: primero
se prueba un nombre, y un número solo se toma cuando nada tiene ese
nombre. Un `+` inicial (`+1000`) significa siempre un número.

Dar un archivo a otra cuenta, o a un grupo del que usted no es miembro,
requiere la capacidad `CAP_FS_CHOWN`; sin ella puede cambiar el grupo de
un archivo suyo a uno de sus propios grupos. Un cambio borra el bit
set-user-ID de un archivo, y su bit set-group-ID cuando su grupo puede
ejecutarlo; un directorio conserva ambos.

Un enlace simbólico nombrado como operando cambia el archivo que nombra,
o con `-h` el propio enlace. Con `-R` se cambian un operando de
directorio y todo lo que contiene, cada directorio después de su
contenido. De un enlace simbólico encontrado dentro del árbol se cambia
el propio enlace, y lo que nombra nunca se toca; con `--dereference`
también el enlace se deja intacto. Con `-R`, de un enlace simbólico
nombrado como operando se cambia el propio enlace, que no se sigue:
`--dereference` cambia en su lugar lo que nombra, y `-H` recorre un
directorio que nombra. Un archivo que no puede alcanzarse o cambiarse se
notifica y los demás se cambian de todos modos. Un informe nombra a cada
propietario y grupo por su nombre, o por su número cuando ninguna cuenta
ni grupo lo tiene. `--` termina el análisis de opciones: cada argumento
posterior es un operando.

## OPTIONS

- `-c, --changes` — informar solo de los archivos cuyo propietario o
  grupo cambió realmente.
- `-f, --silent, --quiet` — suprimir la mayoría de los mensajes de
  error; la ejecución sigue fallando y el estado de salida lo refleja.
- `-v, --verbose` — informar de cada archivo procesado, incluido aquel
  cuyo cambio se rechazó o que no pudo alcanzarse.
- `--dereference` — cambiar lo que nombra un enlace simbólico en lugar
  del enlace; sin `-R` es el comportamiento predeterminado.
- `-h, --no-dereference` — cambiar el propio enlace simbólico en lugar
  de lo que nombra.
- `--preserve-root` — con `-R`, negarse a cambiar `/`, se llegue a él
  como se llegue.
- `--no-preserve-root` — tratar `/` como cualquier otro directorio (el
  comportamiento predeterminado).
- `--from=CURRENT_OWNER:CURRENT_GROUP` — cambiar un archivo solo si su
  propietario y su grupo actuales son estos; omitir cualquiera de los
  dos acepta cualquier valor.
- `--reference=RFILE` — dar a cada archivo el propietario y el grupo de
  `RFILE` en lugar de nombrarlos.
- `-R, --recursive` — cambiar archivos y directorios recursivamente.
- `-H` — con `-R`, recorrer el directorio al que lleva un enlace
  simbólico nombrado como operando.
- `-L` — con `-R`, seguir cada enlace simbólico a un directorio; se
  rechaza, porque un cambio recursivo nunca sigue un enlace dentro del
  árbol.
- `-P` — con `-R`, no seguir ningún enlace simbólico (el comportamiento
  predeterminado). Prevalece el último que se dé de `-H`, `-L` y `-P`.
- `-?, --help` — mostrar la ayuda corta de esta orden.
- `--version` — imprimir el nombre y la versión de la herramienta.

## EXAMPLES

- `chown alice notes.txt` — dar `notes.txt` a `alice`, conservando su
  grupo.
- `chown alice:staff notes.txt` — dárselo a `alice` y al grupo `staff`.
- `chown :staff shared.txt` — cambiar solo el grupo.
- `chown -R alice Projects` — dar el árbol `Projects` a `alice`.
- `chown -R --from=bob alice Shared` — dar a `alice` solo lo que `bob`
  posee en `Shared`.
- `chown --reference=template.txt notes.txt` — dar a `notes.txt` el
  propietario y el grupo de `template.txt`.

## EXIT STATUS

- `0` — todos los cambios tuvieron éxito.
- `1` — un archivo no pudo alcanzarse o cambiarse, o la salida falló;
  cada razón se imprime en la salida de error (suprimida bajo `-f`). Una
  línea de órdenes no entendida, o un propietario, grupo o archivo de
  referencia que no pudo leerse, también termina con `1` y no cambia
  nada.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `chmod`
- `ls`
- `stat`
