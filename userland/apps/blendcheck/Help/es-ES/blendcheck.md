## NAME

blendcheck — comparar la mezcla del motor de pantalla y del escritorio

## SYNOPSIS

`blendcheck`

## DESCRIPTION

Toma el control de la pantalla y muestra un degradado de grises que la
recorre a lo ancho, con tres paneles de cristal oscuro encima, uno sobre
otro:

1. mezclado por el escritorio, con tramado, como el escritorio dibuja
   las ventanas translúcidas;
2. mezclado por el escritorio sin el tramado, lo que muestra los
   escalones que el efecto de bandas deja en un degradado suave;
3. mezclado por el propio hardware de pantalla.

Si el panel inferior se parece al superior, el hardware de pantalla
mezcla las zonas translúcidas sin efecto de bandas. Si muestra los
escalones del panel central, no lo hace. La orden informa también de
si el controlador de la pantalla declara su mezcla libre de bandas.

Pulse cualquier tecla o botón del puntero para devolver la pantalla.

La orden necesita la pantalla para ella sola, así que ejecútela desde
una sesión de texto y no desde un terminal en el escritorio. Una
pantalla sin capas de hardware no tiene una mezcla propia que comparar,
y la orden lo indica.

## OPTIONS

- `-h, -?` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `blendcheck` — mostrar la comparación hasta que se pulse una tecla.

## EXIT STATUS

- `0` — la comparación se mostró y se cerró.
- `1` — la pantalla estaba ocupada por una sesión, no tiene capas de
  hardware, rechazó el patrón, o se perdió el puesto mientras se
  mostraba. Una línea de órdenes no comprendida también termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve
  (una etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
