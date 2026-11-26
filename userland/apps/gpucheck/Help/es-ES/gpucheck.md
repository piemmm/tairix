## NAME

gpucheck — comprobar cada GPU servida, la pantalla que muestra lo que
dibuja una de ellas, y un kernel de cómputo propio

## SYNOPSIS

`gpucheck`

`gpucheck --present`

`gpucheck --shader FILE [--words N]`

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
abandonando el dibujo. A continuación, pide a la GPU que ejecute un
programa sobre más grupos de trabajo de los que admite, a lo que todo
dispositivo debe responder abandonando ese trabajo. Por último, ejecuta
pequeños programas que entre todos usan cada una de las instrucciones
que puede emplear un programa de GPU, y cada respuesta debe ajustarse a
la del dispositivo de software con la precisión que garantiza la
definición de su instrucción; una GPU que no pueda ejecutar alguno de
ellos no queda sujeta a él. Una GPU coincide cuando deja los mismos
bytes que el dispositivo de software, rechaza ese relleno por el mismo
motivo, abandona el mismo trabajo y da la misma respuesta a cada uno de
esos programas.

La orden imprime una línea por cada GPU, que nombra su nodo de hardware:
o bien que la GPU coincide, o bien la primera diferencia encontrada: el
trabajo rechazado, y en qué paso; un byte que se lee distinto; una
respuesta distinta al relleno; un valor que un programa gráfico dejó
distinto; un byte de una imagen que se lee distinto; un byte de un
dibujo que se lee distinto; una respuesta distinta al dibujo a partir de
una lista; una respuesta distinta al trabajo más allá de los límites de
la GPU; o una instrucción, en uno de los pequeños programas, cuya
respuesta se aleja de la del dispositivo de software más de lo que
permite su definición.

`sysinfo gpu` enumera las GPU que comprueba la orden.

Con `--present`, la orden muestra en su lugar una imagen conocida: un
fondo azul, una franja de colores, una fila de baldosas a cuadros y una
baldosa copiada en la esquina opuesta, en una ventana que cubre toda la
pantalla. La imagen la dibuja la primera GPU que sirve un controlador o,
si no se sirve ninguna, el dispositivo de software. La orden la presenta
una vez por fotograma hasta que se pulse Escape o se cierre la ventana,
y luego imprime cuántos fotogramas presentó, qué los dibujó y si la
pantalla los mostró todos.

Con `--shader`, la orden ejecuta en su lugar un programa propio: el
kernel de cómputo contenido en FILE, escrito en WGSL o compilado a
SPIR-V, formas que se distinguen por cómo empieza el archivo. El WGSL se
compila en un proceso aparte que no puede alcanzar nada más, y
cualquiera de las dos formas se comprueba antes de entregarla a una GPU.
Un kernel rechazado no se ejecuta: se imprime el rechazo y, en WGSL, la
línea y la columna en que se sitúa. El kernel debe tener un único punto
de entrada de cómputo, que use un único búfer de almacenamiento en el
grupo 0, enlace 0. El búfer contiene N palabras, 256 salvo que `--words`
indique otra cosa, con la palabra *i* valiendo *i* al comienzo, y el
kernel se invoca una vez por cada palabra. Se ejecuta en cada GPU que
sirve un controlador y en el dispositivo de software; a su lado, en
memoria que pertenece a un programa aparte, la GPU guarda una marca que
el búfer del kernel nunca debe llegar a contener. Para cada GPU imprime
o bien que ambos coinciden —el búfer se lee igual, o ambos abandonaron
un kernel que superó su tiempo límite y siguieron sirviendo—, o bien la
primera diferencia: una palabra que se lee distinto, la marca encontrada
en el búfer, el kernel rechazado, abandonado o sin respuesta cuando el
dispositivo de software sí lo ejecutó, o una GPU que dejó de servir tras
haberlo abandonado.

## OPTIONS

- `--present` — mostrar la imagen a pantalla completa y comprobar que la
  pantalla mostró cada fotograma presentado.
- `--shader FILE` — ejecutar el kernel de cómputo contenido en FILE,
  código fuente WGSL o un módulo SPIR-V, en cada GPU servida y en el
  dispositivo de software.
- `--words N` — con `--shader`, cuántas palabras contiene el búfer del
  kernel: de 1 a 65536, y 256 si no se indica.
- `-h, -?` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `gpucheck` — comprobar cada GPU servida.
- `gpucheck --present` — mostrar la imagen a pantalla completa; pulsar
  Escape para terminar.
- `gpucheck --shader double.wgsl` — ejecutar un kernel WGSL sobre
  256 palabras en cada GPU servida.
- `gpucheck --shader kernel.spv --words 4096` — ejecutar un kernel
  compilado sobre 4096 palabras.

## EXIT STATUS

- `0` — cada GPU servida coincidió con el dispositivo de software; con
  `--present`, la pantalla mostró cada fotograma presentado; con
  `--shader`, cada GPU servida coincidió con el dispositivo de software
  en el kernel.
- `1` — una GPU difirió o no se pudo alcanzar, no se sirve ninguna GPU,
  no se pudieron enumerar las GPU servidas, o el dispositivo de
  software no pudo ejecutar el trabajo; con `--present`, un fotograma no
  se mostró, no se presentó ninguno, o la ventana o el dispositivo
  rechazó un paso; con `--shader`, no se pudo leer el archivo, se
  rechazó el kernel, o una GPU difirió. Una línea de órdenes no
  comprendida también termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve
  (una etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
