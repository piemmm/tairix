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
que la GPU reserva para sí misma y de vuelta, y luego pide un relleno
más allá del final de un búfer, que todo dispositivo debe rechazar.
Después ejecuta cada uno de los programas gráficos propios del sistema
sobre una imagen conocida. Si la GPU puede albergar todos los tipos de
imagen que usa el paso siguiente, el trabajo copia imágenes a la
memoria, desde ella y de una imagen a otra, las borra y ejecuta un
programa que lee, escribe y muestrea imágenes. Si la GPU puede dibujar,
el trabajo dibuja en imágenes formas, puntos y una línea de colores
conocidos, ocultando lo que queda detrás, omitiendo las formas que están
de espaldas, mezclando colores y dejando algunos intactos, y reúne
cuatro muestras de cada píxel en una. También dibuja a partir de una
lista en memoria que nombra la primera copia de una forma que se va a
dibujar, a lo que una GPU que no pueda hacerlo debe responder
abandonando el dibujo. Por último, pide a la GPU que ejecute un programa
sobre más grupos de trabajo de los que admite, a lo que todo dispositivo
debe responder abandonando ese trabajo. Una GPU coincide cuando deja los
mismos bytes que el dispositivo de software, rechaza ese relleno por el
mismo motivo y abandona el mismo trabajo.

La orden imprime una línea por cada GPU, que nombra su nodo de hardware:
o bien que la GPU coincide, o bien la primera diferencia encontrada: el
trabajo rechazado, y en qué paso; un byte que se lee distinto; una
respuesta distinta al relleno; un valor que un programa gráfico dejó
distinto; un byte de una imagen que se lee distinto; un byte de un
dibujo que se lee distinto; una respuesta distinta al dibujo a partir de
una lista; o una respuesta distinta al trabajo más allá de los límites
de la GPU.

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
