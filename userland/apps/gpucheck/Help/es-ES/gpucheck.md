## NAME

gpucheck — comprobar cada GPU servida frente al dispositivo de software

## SYNOPSIS

`gpucheck`

## DESCRIPTION

Ejecuta el mismo trabajo breve en cada GPU que sirve un controlador y en
el dispositivo de software del sistema, e indica para cada GPU si ambos
coinciden.

El trabajo escribe un patrón conocido en memoria que la orden puede
leer, rellena la memoria contigua, copia ambas cosas a través de memoria
que la GPU reserva para sí misma y de vuelta, y por último pide un
relleno más allá del final de un búfer, que todo dispositivo debe
rechazar. Una GPU coincide cuando deja los mismos bytes que el
dispositivo de software y rechaza ese relleno por el mismo motivo.

La orden imprime una línea por cada GPU, que nombra su nodo de hardware:
o bien que la GPU coincide, o bien la primera diferencia encontrada: un
byte que se lee distinto, el trabajo rechazado o una respuesta distinta
al relleno.

`sysinfo gpu` enumera las GPU que comprueba la orden.

## OPTIONS

- `-h, -?` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `gpucheck` — comprobar cada GPU servida.

## EXIT STATUS

- `0` — cada GPU servida coincidió con el dispositivo de software.
- `1` — una GPU difirió o no se pudo alcanzar, no se sirve ninguna GPU,
  no se pudieron enumerar las GPU servidas, o el dispositivo de
  software no pudo ejecutar el trabajo. Una línea de órdenes no comprendida también
  termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve
  (una etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
