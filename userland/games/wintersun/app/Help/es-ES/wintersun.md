## NAME

wintersun — recorrer un mundo invernal generado proceduralmente

## SYNOPSIS

`wintersun [--reference-scene]`

## DESCRIPTION

Abre una ventana de escritorio sobre un mundo generado: una vista cenital de
un terreno que la máquina sintetiza en lugar de distribuir, iluminado por un
sol bajo que proyecta sombras largas por cada ladera.

Nada de este mundo se almacena como ilustración. Cada material del que está
hecho el suelo — nieve, roca, grava, brezal, tundra — es un puñado de números
que el cliente convierte en textura al dibujar, de modo que el mundo se ve
igual en todas las máquinas y casi no ocupa espacio en disco. Los caminos se
desgastan sobre lo que cruzan en lugar de posarse encima.

El terreno que aún no se ha generado se dibuja como el hueco que es y se
rellena a medida que llega. El cliente dibuja lo que tiene en lugar de
detenerse a esperar, así que la ventana sigue respondiendo mientras el mundo
se pone al día.

Las flechas o `W`, `A`, `S`, `D` caminan. Dos teclas mantenidas a la vez
recorren la diagonal entre ellas a la misma velocidad, y las teclas opuestas
se anulan. La vista le sigue y se detiene en el borde del mundo en lugar de
salirse de él. Las laderas demasiado empinadas y el agua demasiado profunda le
desvían.

`+` y `-` acercan y alejan la vista, en cinco pasos, desde una celda del mundo
de ocho píxeles de ancho hasta una de ciento veintiocho.

`F11` pone la ventana en pantalla completa y la devuelve después a como
estaba: una ventana maximizada vuelve maximizada. `Esc` la restaura. `Q` sale.

Cada detalle se dibuja en su nivel más fino hasta que elija otra cosa. La fila
*Settings…* del menú del juego en la barra de iconos abre su ventana de
ajustes, donde la calidad es *Ultra*, cada detalle en su nivel más fino;
*Basic*, cada detalle en su nivel más sencillo al tamaño completo de la
ventana; *Custom*, su propia elección de la iluminación, las sombras, la
textura del suelo y la escala de renderizado, cada una en su propio control
deslizante; o *Auto*. Mover un control deslizante hace que la elección sea
*Custom*. Su elección se conserva para la próxima vez que juegue.

En *Auto* el cliente reduce el detalle cuando los fotogramas llevan un rato
llegando tarde — primero la iluminación, luego las sombras, luego la escala de
renderizado — y lo devuelve, paso a paso, a medida que se recuperan. Juzga a
lo largo de segundos y no de fotogramas sueltos, así que un momento de otro
trabajo en la máquina no cuesta nada y una ventana mayor no lo lleva a su
nivel más sencillo, y nunca dibuja las figuras demasiado pequeñas para
leerlas.

Una ventana mayor de lo que el renderizador por software puede llenar se
dibuja a 2560×1440 como máximo y se escala hasta el tamaño de la ventana.

## OPTIONS

- `-h, -?, --help` — mostrar la ayuda corta de este comando.
- `--reference-scene` — dibujar la escena de referencia fija y mantenerla
  quieta: un mismo mundo, los mismos personajes y el mismo instante, idénticos
  en todas las máquinas, para que una imagen de la ventana pueda compararse
  con otra dibujada en otro lugar. `F11` y `Esc` siguen cambiando el tamaño de
  la ventana; nada más se mueve.

## EXIT STATUS

`0` al salir. Un estado distinto de cero indica su motivo en la salida de
error: el mundo no se pudo generar, la ventana no se pudo abrir, o se perdió
el canal de eventos de la sesión.

- `2` — la línea de comandos no se entendió.
- `87` — la escena de referencia no se pudo dibujar.

## SEE ALSO

`sapper`
