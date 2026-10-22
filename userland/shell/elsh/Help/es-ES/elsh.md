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

- `cd <path>`, `pwd` — cambiar e imprimir el directorio de trabajo.
- `echo ...` — imprimir sus operandos.
- `export NAME=value`, `unset NAME` — editar el entorno exportado.
- `jobs`, `fg`, `bg` — control de tareas.
- `ulimit` — leer e imponer límites de recursos.
- `elevate` — ejecutar un comando re-autenticado a través del
  supervisor de inicio de sesión de la consola.
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

El intérprete no acepta operandos: la ejecución de guiones aún no forma
parte de su gramática.

En un terminal, el shell ofrece un editor de línea interactivo:
Arriba/Abajo recorren el historial de órdenes, `Ctrl-R` lo busca,
`Ctrl-C` descarta la línea en edición, `Ctrl-D` en una línea vacía
termina la sesión, y Tab completa nombres de órdenes, rutas y
referencias de recursos como `sys:random`. Un espacio de nombres completa
sus selectores registrados segmento a segmento
(`state:` → `net/` → `wan/` → `link`). Donde el registro solo conoce la
forma —un nombre de interfaz, una línea de interrupción—, Tab ofrece los
nombres reales de la máquina, o nada si esta sesión no puede enumerarlos. `info:`/`state:`/`stats:` aparecen como argumento y tras `<`, que lee el
valor (`cat < info:mem/physical`), pero nunca tras `>`: tal recurso nunca se
escribe por redirección.

## OPTIONS

- `-h, -?` — mostrar la ayuda corta de este comando y salir.

## EXIT STATUS

- El código del comando integrado `exit`, o `0` cuando el flujo de
  entrada termina (o se mostró la ayuda corta).
- `2` — la invocación no fue comprendida.

## ENVIRONMENT

- `PATH` — los directorios buscados después del prefijo fijo de
  tiendas.
- `GLOBIGNORE` — patrones separados por `:` cuyas coincidencias descarta
  un patrón de nombres de archivo.
- `LANG` — la configuración regional preferida para la ayuda corta (una
  etiqueta BCP-47 como `es-ES`), exportada a cada comando lanzado.

## SEE ALSO

- `man`
