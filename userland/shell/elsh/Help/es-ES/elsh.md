## NAME

elsh — el intérprete de comandos de TAIRiX

## SYNOPSIS

`elsh [-h | -?]`

## DESCRIPTION

Ejecuta un intérprete de comandos interactivo — un bucle
leer-evaluar-imprimir sobre los flujos estándar heredados. Una palabra
de comando escrita se resuelve primero contra los comandos integrados
del intérprete, luego en la tienda de comandos del sistema
(`/System/Commands`), la tienda de aplicaciones del sistema
(`/System/Applications`), la tienda de comandos (`<home>/Commands`) y
la tienda de aplicaciones (`<home>/Applications`) del propio usuario, y
después en los directorios de la variable `PATH`; esas cuatro tiendas
forman un prefijo fijo que el usuario no puede reordenar ni anular, de
modo que `PATH` nunca puede ocultar un comando del sistema. Una palabra
no resuelta sale con `127`; un paquete resuelto pero no ejecutable sale
con `126`.

Los comandos integrados:

- `cd [-L | -P [-e]] [directory]`, `cd -` — cambiar el directorio de
  trabajo: a `directory`, a `HOME` si no se indica ninguno, o con `-` de
  vuelta a `OLDPWD`, imprimiéndolo. `-L`, la opción predeterminada,
  aplica `directory` a `PWD` tal como está escrito mientras `PWD` aún
  nombre el directorio de trabajo, de modo que `cd link` seguido de
  `cd ..` vuelve a donde está el enlace; `-P` sigue primero los enlaces,
  y `PWD` pasa a ser la ruta propia del directorio (`-e` falla entonces
  cuando esa ruta no puede leerse). `/..` es `/`, y bajo una raíz de
  alias (`Home:/`) un `..` nunca sube por encima de ella.
- `pwd [-L | -P]` — imprimir el directorio de trabajo: `PWD` mientras
  aún lo nombre (`-L`, la opción predeterminada), o la ruta propia del
  directorio (`-P`).
- `echo ...` — imprimir sus operandos.
- `export NAME=value`, `unset NAME` — editar el entorno exportado.
- `jobs`, `fg`, `bg` — control de tareas.
- `ulimit [-a] [-H | -S] [resource [value]]` — leer e imponer límites de
  recursos; `ulimit -n [value]` lee o ajusta el límite de archivos abiertos.
- `umask [-S] [mode]` — mostrar la máscara de creación de modos de
  archivo (`0022`), o con `-S` los permisos que deja
  (`u=rwx,g=rx,o=rx`), o fijarla: un `mode` octal es la propia máscara,
  y uno simbólico (`g-w`, `u=rwx,g=rx,o=`) edita los permisos que deja.
  Cada comando lanzado después crea sus archivos bajo ella, y una
  redirección crea su archivo con `0666` menos la máscara.
- `elevate <user> <program> [argument ...]` — ejecutar un solo comando,
  con los argumentos dados, re-autenticado a través del supervisor de
  inicio de sesión de la consola.
- `shopt [-pqsu] [-o] [name ...]` — mostrar o ajustar las opciones de
  los patrones de nombres de archivo.
- `help` — listar los comandos integrados.
- `exit [code]` — terminar la sesión.

Una palabra que contiene un `*`, `?` o `[...]` sin entrecomillar es un
patrón de nombres de archivo, sustituido, como en bash, por los nombres
ordenados que coinciden con él: `cp *.tmp /tmp`. Un `.` inicial solo
coincide con un `.`, y un patrón sin coincidencias se queda tal como se
escribió. Las llaves se expanden primero: `a{b,c}` da `ab ac`, y
`{1..5}` cuenta. Entrecomillar hace literal cualquiera de estos
caracteres, y el valor de una variable nunca se lee como patrón.
`shopt -s` activa las opciones de bash: `nullglob` (sin coincidencias no
queda palabra), `failglob` (sin coincidencias es un error), `dotglob`,
`nocaseglob`, `globstar` (`**` coincide a cualquier profundidad) y
`extglob` (`?(…)`, `*(…)`, `+(…)`, `@(…)`, `!(…)`); `shopt -o -s noglob`
desactiva los patrones de nombres de archivo.

En un terminal, el intérprete ofrece un editor de línea interactivo con
las teclas habituales de bash y zsh: Arriba/Abajo (o `Ctrl-P`/`Ctrl-N`)
recorren el historial de comandos de la sesión, `Ctrl-R` busca en él de
forma incremental, Izquierda/Derecha/Inicio/Fin y `Ctrl-A`/`Ctrl-E`
mueven el cursor, `Ctrl-K`/`Ctrl-U`/`Ctrl-W`/`Ctrl-Y` cortan y pegan,
`Ctrl-T` intercambia dos caracteres, `Ctrl-L` redibuja sobre una
pantalla limpia, `Ctrl-C` descarta la línea en edición, y `Ctrl-D` en
una línea vacía termina la sesión. Tab completa la palabra bajo el
cursor: nombres de comandos (comandos integrados y aplicaciones de
comando instaladas), rutas de archivo y —para un destino de redirección
o una palabra con forma de referencia— referencias de recursos como
`sys:random`. Un espacio de nombres completa sus selectores
registrados segmento a segmento (`state:` → `net/` → `wan/` → `link`);
donde el registro solo conoce la forma —un nombre de interfaz, una
línea de interrupción—, Tab ofrece los nombres reales de la máquina, o
nada si esta sesión no puede enumerarlos. `info:`/`state:`/`stats:`
aparecen como argumentos y tras `<`, que lee el valor
(`cat < info:mem/physical`), pero nunca tras `>`: tal recurso nunca se
escribe por redirección. La entrada por tubería o desde un guion no
pasa por el editor y se comporta igual con o sin terminal.

El intérprete no acepta operandos: la ejecución de guiones aún no forma
parte de su gramática.

## OPTIONS

- `-h, -?` — mostrar la ayuda corta de este comando y salir.

## EXIT STATUS

- El código del comando integrado `exit`, o `0` cuando el flujo de
  entrada termina (o se mostró la ayuda corta).
- `2` — la invocación no fue comprendida.

## ENVIRONMENT

- `HOME` — adonde va `cd` sin operando.
- `PWD`, `OLDPWD` — el directorio de trabajo tal como `cd` llegó a él, y
  el anterior; `cd` fija ambos. Un `PWD` con el que arranca el
  intérprete se conserva mientras nombre el directorio de trabajo y, de
  lo contrario, pasa a ser la ruta propia de ese directorio.
- `PATH` — los directorios buscados después del prefijo fijo de
  tiendas.
- `GLOBIGNORE` — patrones separados por `:` cuyas coincidencias descarta
  un patrón de nombres de archivo. Mientras la variable no esté vacía, un
  patrón de nombres de archivo coincide también con los nombres que empiezan
  por `.`, así que `rm *` borra entonces también `.profile`.
- `LANG` — la configuración regional preferida para la ayuda corta (una
  etiqueta BCP-47 como `es-ES`), exportada a cada comando lanzado.

## SEE ALSO

- `man`
