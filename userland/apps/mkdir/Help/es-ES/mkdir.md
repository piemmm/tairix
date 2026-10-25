## NAME

mkdir — crear directorios

## SYNOPSIS

`mkdir [-pv] [-m MODE] [--] directorio...`

## DESCRIPTION

Crea cada directorio operando, en orden. Sin `-p`, el directorio padre
de cada operando debe existir ya y el operando mismo no debe existir;
un operando que no puede crearse se notifica y el siguiente se crea de
todos modos.

Con `-p` se crea primero cada ancestro que falte, del más externo al
más interno, y un operando (o ancestro) que ya exista como directorio
no es un error. Un ancestro que exista como fichero sigue fallando:
nada se reemplaza nunca en silencio.

Un directorio nuevo recibe el modo `a=rwx` menos la umask. Con `-m`,
cada directorio operando creado recibe en su lugar exactamente `MODE`,
sea cual sea la umask: un valor octal o cláusulas simbólicas, tal como
las lee `chmod`, partiendo de `a=rwx`. Un ancestro que falte, creado
por `-p`, conserva los permisos de escritura y búsqueda de su
propietario, diga lo que diga la umask, de modo que el directorio que
va debajo siempre puede crearse. Un directorio creado dentro de un
directorio set-group-ID conserva el bit que hereda salvo que `MODE` lo
nombre. Un operando que ya existe conserva su modo. `--` termina el
análisis de opciones: cada argumento posterior es una ruta.

## OPTIONS

- `-m, --mode=MODE` — dar a cada directorio operando creado el modo
  `MODE`, octal o simbólico como en `chmod`, en lugar de `a=rwx`
  menos la umask.
- `-p, --parents` — crear los directorios padres que falten; un
  operando que ya es un directorio no es un error.
- `-v, --verbose` — informar de cada directorio creado como
  `mkdir: created directory 'dir'`.
- `-h, -?` — mostrar la ayuda corta de esta orden (también `--help`).

## EXAMPLES

- `mkdir Notes` — crear un directorio en el directorio actual.
- `mkdir -p Projects/os/build` — crear toda la cadena, omitiendo las
  partes que ya existen.
- `mkdir -pv Home:/tools/bin` — crear bajo una raíz de alias,
  informando de cada directorio nuevo.
- `mkdir -m 700 Private` — crear un directorio que solo su propietario
  pueda usar.

## EXIT STATUS

- `0` — todos los directorios se crearon (o, con `-p`, ya existían).
- `1` — un fallo del sistema de ficheros o de la salida, o un modo que
  no pudo asignarse; la razón se imprime en la salida de error. Una
  línea de órdenes no entendida, o un `MODE` que no es un modo, también
  termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta (una
  etiqueta BCP-47 como `es-ES`).

## SEE ALSO

chmod, rmdir, rm, ls
