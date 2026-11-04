## NAME

clear — borrar la pantalla del terminal

## SYNOPSIS

`clear [-x]`

## DESCRIPTION

Escribe la secuencia que sitúa el cursor en la esquina superior
izquierda y borra toda la pantalla, dejándola vacía. La secuencia
emitida depende del terminal nombrado en `TERM`; un terminal que no
puede borrar (un `TERM` desconocido degrada al perfil mínimo) hace que
la orden falle en lugar de imprimir bytes que el terminal mostraría
como caracteres extraños.

Si el terminal lo permite, también se borran las líneas que conserva
por encima de la pantalla —el historial de desplazamiento—, de modo que
nada de lo que había en la pantalla puede volver a verse desplazándose
hacia atrás.

## OPTIONS

- `-x` — conservar el historial: borrar solo la pantalla.
- `-h, -?` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `clear` — borrar la pantalla.

## EXIT STATUS

- `0` — la secuencia de borrado fue escrita.
- `1` — el terminal no puede borrar, o la salida no pudo entregarse.
  Una línea de órdenes no comprendida también termina con `1`.

## ENVIRONMENT

- `TERM` — el terminal cuya secuencia de borrado se escribe.
- `LANG` — la configuración regional preferida para la ayuda breve
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `reset`
- `man`
