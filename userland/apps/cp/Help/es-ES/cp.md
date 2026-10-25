## NAME

cp — copiar archivos y directorios

## SYNOPSIS

`cp [-dfHilLnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

## DESCRIPTION

Copia cada operando de origen a un destino. Con un único origen y un
destino que no nombra un directorio, el origen se copia a esa ruta
exacta. Cuando el destino nombra un directorio existente — y siempre
que haya más de un origen — cada origen se copia *dentro* de ese
directorio bajo su propio nombre base.

Un origen que es directorio solo se copia con `-r`, que reproduce el
subárbol completo; sin `-r` un operando de directorio se rechaza. Un
archivo de destino existente se sobrescribe por defecto, se omite con
`-n` y se pregunta por la salida de error estándar con `-i` (una
pregunta rechazada omite esa copia y hace fallar la ejecución, como en
GNU; una respuesta ilegible nunca se considera consentimiento).
Un destino que es el propio archivo de origen —el mismo nombre, otro
nombre suyo o un enlace a él— se rechaza por ser el mismo archivo y
queda como estaba, salvo con `-l`, que lo da por ya enlazado. Un enlace
que `-f` pone en el lugar de un nombre existente ocupa ese lugar en un
solo paso y se crea antes de reemplazar nada, de modo que uno que no
puede crearse deja intacto el nombre.

Un archivo que no puede leerse, crearse o escribirse se notifica y los
demás se copian de todos modos, también el resto de su directorio. `--`
termina el análisis de opciones: cada argumento posterior es una ruta.

Un archivo nuevo se crea con los bits de permiso de su origen, sin los
bits set-ID ni sticky, menos la umask, de modo que la copia de un
archivo privado sigue siendo privada; un destino que ya existe conserva
su propio modo. Un directorio nuevo termina con los bits de permiso y
sticky de su origen menos la umask; mientras se copian sus entradas en
él, queda abierto a su propietario, de modo que un directorio de origen
de solo lectura se copia entero de todos modos, igual que bajo una umask
que niega la escritura al propietario. Nunca se escribe a través de un
enlace simbólico encontrado dentro de un árbol de destino.

El `-a`/`--archive` de GNU y los conjuntos de `--preserve` distintos de
`--preserve=links,xattr` se **rechazan**, no se restringen. `-a`
equivale a `-dR --preserve=all`, y `--preserve=all` incluye las marcas
de tiempo de un nodo, que ninguna llamada de este sistema puede
establecer — así que atender `-a` notificaría una conservación que no ha
ocurrido. Use `-dR` para el resto.

Cada copia lleva los atributos extendidos del origen —un tipo de archivo
de RISC OS, el tipo y el creador de un Mac— allí donde el destino puede
guardarlos, y conserva como huecos los huecos de un archivo disperso.
Los atributos que un destino no puede guardar se notifican en un único
aviso al terminar la ejecución, y en un registro `omission` en el flujo
de información estándar; con `--preserve=xattr` esa copia falla en su
lugar.

Un directorio nunca se copia dentro de sí mismo: un destino dentro del
origen — se escriba como se escriba, a través de un enlace, un `..` u
otro nombre del volumen — se rechaza antes de escribir nada, y un
directorio movido al origen mientras dura la copia detiene la copia
donde el recorrido lo encuentra.

## OPTIONS

- `-r, -R, --recursive` — copiar directorios y su contenido.
- `-f, --force` — cuando un archivo de destino no puede crearse,
  eliminarlo y reintentar la copia una vez; con `-l`, `-s` o un enlace
  que `-P` reproduce, poner el enlace en el lugar de un nombre
  existente.
- `-i, --interactive` — preguntar antes de sobrescribir un archivo
  existente; solo consiente una respuesta que empiece por `y`/`Y`, y
  una pregunta rechazada omite ese archivo y hace fallar la ejecución.
- `-n, --no-clobber` — no sobrescribir nunca un archivo existente. El
  último de `-i` y `-n` gana.
- `-l, --link` — dar al destino un segundo nombre del nodo de la fuente
  en lugar de copiar sus bytes, de modo que los dos nombres no puedan
  divergir en una escritura posterior. Una fuente que es directorio
  sigue necesitando `-r`.
- `-s, --symbolic-link` — crear un enlace simbólico que nombre la fuente
  en lugar de copiarla.
- `-P, --no-dereference` — reproducir cada enlace simbólico de las
  fuentes como un enlace con el mismo destino, literalmente, en lugar de
  copiar aquello a lo que apunta (así un enlace relativo o colgante
  sobrevive a la copia).
- `-L, --dereference` — seguir cada enlace simbólico de las fuentes y
  copiar aquello a lo que apunta.
- `-H` — seguir un enlace simbólico nombrado como operando, pero
  reproducir los que aparecen dentro de un árbol. Sin `-H`, `-L` ni
  `-P`, una copia recursiva reproduce cada enlace, también los de sus
  operandos, y cualquier otra copia sigue los enlaces de sus operandos,
  como la de GNU; `-l` enlaza aquello a lo que apunta un enlace seguido.
- `--preserve=links` — dos fuentes que nombran un mismo nodo obtienen
  dos nombres en el destino en lugar de dos copias, de modo que la copia
  no duplica el almacenamiento en silencio.
- `--preserve=xattr` — todos los atributos extendidos deben copiarse:
  una copia cuyo destino no pueda guardar uno falla y lo nombra.
  `--preserve=links,xattr` pide ambas cosas.
- `--no-preserve=xattr` — no copiar ningún atributo extendido. También
  se aceptan `ownership` y `timestamps`, que de por sí no se conservan.
- `--no-preserve=mode` — crear cada archivo y cada directorio nuevos con
  el modo que toma uno nuevo cuando nada lo indica, menos la umask, en
  lugar del de su origen. `--no-preserve=all` también lo hace.
- `--sparse=WHEN` — cómo se copia un archivo disperso: `auto`, el valor
  predeterminado, conserva sus huecos; `always` además deja sin escribir
  cada bloque de ceros que almacena; `never` escribe cada byte.
- `-d` — `-P` y `--preserve=links` juntas, como en la herramienta GNU.
- `-v, --verbose` — informar de cada copia como `'source' -> 'dest'`.
- `-t dir, --target-directory=dir` — copiar cada origen dentro de
  `dir`, que debe ser un directorio existente. El valor sigue adjunto
  (`-tdir`, `--target-directory=dir`) o como argumento siguiente.
- `-T, --no-target-directory` — tratar el destino como un archivo
  normal; se permite exactamente un origen. No puede combinarse con
  `-t`.
- `-h, -?, --help` — mostrar la ayuda corta de esta orden.

## EXAMPLES

- `cp notes.txt backup.txt` — copiar un archivo con un nombre nuevo.
- `cp -r Projects Archive` — reproducir el árbol `Projects` dentro de
  `Archive` (o como `Archive` si no existe).
- `cp -v -t Backup a.txt b.txt` — copiar ambos archivos a `Backup`,
  informando de cada copia.

## EXIT STATUS

- `0` — todas las copias tuvieron éxito (una omisión por `-n` no es un
  fallo).
- `1` — un archivo no pudo copiarse, se rechazó una pregunta `-i` o
  falló una pregunta o la salida; el motivo de cada fallo se imprime en
  la salida de error estándar. Una línea de órdenes no entendida
  también termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
