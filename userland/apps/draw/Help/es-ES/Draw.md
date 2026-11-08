## NAME

Draw — editor de dibujos vectoriales

## SYNOPSIS

`Draw`

## DESCRIPTION

Dibuja y edita dibujos vectoriales en una ventana del escritorio: trazados,
rectángulos, elipses, estrellas y polígonos, líneas, curvas a mano alzada y
texto, en grupos y capas, sobre una página. Lanzado con un documento — desde
el gestor de archivos, desde el escritorio, o soltando un archivo sobre su
icono en la barra de iconos — abre una ventana con él. Lanzado solo, abre un
dibujo nuevo sobre una página A4. Cada documento es una ventana del único
programa; cerrar la última lo deja en la barra de iconos, y la fila Salir de
su menú de icono lo termina.

Draw abre y guarda SVG, y abre archivos Draw de RISC OS, que guarda como SVG.
Un dibujo conserva su página y sus unidades, sus capas, grupos y recortes,
cada trazado, forma, texto e imagen, los colores lisos y los degradados
lineales y radiales, la opacidad de cada objeto, los guiones, uniones, remates
y puntas de flecha, las guías y la cuadrícula, y el título del dibujo. Un
archivo que contiene lo que un dibujo no puede conservar — efectos de filtro,
máscaras, rellenos de patrón, marcadores, imágenes ilegibles, estilos o
trayectos de texto que un texto no puede guardar, animación o scripts, entre
otros — se abre solo para lectura, con la banda de estado nombrando lo que
contiene, y Guardar pide un nombre nuevo para que el original nunca se
sobrescriba. Guardar una copia escribe el dibujo como PDF; como EPS, con su
transparencia hecha opaca, como la pinta PostScript; como PNG, JPEG o TIFF, a
la resolución de copia de la página y hasta 8192 píxeles en su lado más largo;
como SVG; o como archivo Draw de RISC OS, nombrando lo que el formato no pudo
guardar del dibujo, como los degradados; todas sin tocar el archivo propio del
documento.

La caja de herramientas baja por la izquierda de la ventana, el inspector por
la derecha, y la banda de estado corre a su pie: muestra la posición del
puntero y el tamaño de la selección en las unidades de la página, y dice lo
que hizo la última orden. Arriba, la barra dice cómo se usa la herramienta
en mano y muestra el zoom: pulsar el zoom abre un menú de zooms. Reglas en
las unidades de la página bordean el lienzo por arriba y por la izquierda, y
barras de desplazamiento por la derecha y por abajo. Detenerse sobre el botón
de una herramienta la nombra, junto con la tecla que la elige.

Las herramientas, y las teclas que las eligen, son: selección (`V`), nodos
(`A`), rectángulo (`R`), elipse (`E`), estrella y polígono (`S`), línea (`L`),
pluma (`B`), lápiz (`P`), texto (`T`), degradado (`G`), cuentagotas (`I`),
zoom (`Z`) y mano (`H`).

La herramienta de selección toma un objeto con una pulsación, un grupo
entero, y Mayús añade un objeto a la selección o lo quita. Una pulsación en
el lienzo vacío arrastra un marco que selecciona lo que queda por completo
dentro de él, o con Alt todo lo que toca. Arrastrar la selección la mueve,
a lo largo de un eje con Mayús, y con Alt mueve una copia. Sus ocho tiradores
la escalan desde el tirador opuesto, una esquina con Mayús conservando sus
proporciones; arrastrar justo por fuera de una esquina la gira sobre su
centro, con Mayús en pasos de 15 grados. Las teclas de flecha la mueven un
píxel, y diez con Mayús.

La herramienta de nodos muestra los nodos del trazado seleccionado y los
tiradores de sus curvas: arrastre un nodo para moverlo, o un tirador para
doblar su curva; el otro tirador de un nodo suave sigue alineado. Los
rectángulos, elipses, estrellas y líneas se dibujan arrastrando: Mayús iguala
los lados — un cuadrado, un círculo, una línea en un múltiplo de 15 grados —
y Alt dibuja desde donde empezó el arrastre. Una estrella se dibuja desde su
centro; sus puntas, su radio interior y si es un polígono se fijan en la
pestaña Forma del inspector, igual que las esquinas de un rectángulo nuevo.

La pluma traza un camino nodo a nodo: haga clic para una esquina, o arrastre
para un nodo suave y su curva. Un clic sobre el primer nodo cierra el trazado,
e Intro, o elegir otra herramienta, lo termina abierto; `Delete` o `Backspace`
retira el último nodo, y Escape descarta el trazado. El lápiz dibuja a mano
alzada, y su línea se ajusta con curvas suaves al soltarlo. La herramienta de
texto empieza un texto donde se hace clic, o edita el texto pulsado: escriba,
Intro empieza una línea, las teclas de flecha, Inicio y Fin mueven el cursor,
y Escape, o un clic en otro sitio, termina la escritura como un solo cambio;
el texto que queda vacío se elimina. El cuentagotas da a la selección, y a lo
que se dibuje después, el estilo del objeto pulsado. La herramienta de
degradado pone un degradado a lo largo del arrastre en la selección o, sin
nada seleccionado, en el objeto pulsado: Mayús lo gira en pasos de 15°, y Alt,
o un degradado ya radial, lo pone radial en torno a donde empezó el arrastre;
mientras la herramienta está en mano, el degradado del objeto seleccionado se
ve como una línea entre dos tiradores. La herramienta de zoom acerca donde se
hace clic, aleja con Alt, y un recuadro arrastrado con ella llena la ventana.
La mano arrastra el lienzo, como lo hace un arrastre con el botón central o
con Espacio pulsado, con cualquier herramienta.

Pulsada sobre un texto, la herramienta de texto pone el cursor donde se pulsó.
Mayúsculas con las flechas, Inicio y Fin, o un arrastre por el texto,
selecciona caracteres, y `Ctrl+A` los selecciona todos; lo que se teclee
entonces ocupa su lugar, en el estilo del primero de ellos. Mientras hay
caracteres seleccionados, una fuente, un tamaño, la negrita, la cursiva, el
espaciado entre letras, la elevación o un color elegidos en el inspector los
cambian solo a ellos; sin ninguno seleccionado, cambian todo el texto, cada
uno de sus caracteres. La pestaña Forma guarda el espaciado entre letras de un
texto, cuánto se elevan sus caracteres, como porcentaje de su tamaño, y su
interlineado —o, en un texto a lo largo de un trazado, a qué distancia del
trazado empieza—, y mientras se teclea el inspector muestra cómo es el primer
carácter seleccionado. Un degradado pinta siempre un texto entero.

Trazado ▸ Poner texto en trazado, con un texto y una forma seleccionados, pone
el texto a lo largo del contorno de la forma, que se une a él: cada carácter
se apoya en el trazado girado según su dirección, un texto a lo largo de un
trazado es una sola línea y un carácter que cae fuera de un extremo no se
dibuja. Soltar del trazado vuelve a poner el texto en líneas desde donde
empezaba su trazado y devuelve el trazado como un objeto propio. Objeto a
trazado convierte un texto pintado de más de una manera en un grupo de un
trazado por cada manera. La herramienta de nodos edita el trazado a lo largo
del cual va un texto como edita cualquier trazado.

La rueda desplaza el lienzo, en horizontal con Mayús; con Ctrl acerca o aleja
alrededor del puntero, como un pellizco en un panel táctil o una pantalla
táctil. Ver ▸ Ajustar página y Ajustar selección las encajan en la ventana, y
Tamaño real muestra el dibujo al 100 por ciento; el zoom va del 1 por ciento
al 25600.

El inspector tiene cinco pestañas. Estilo fija el relleno y el contorno de la
selección — y, sin nada seleccionado, de lo que se dibuje después: elija
Relleno o Contorno, luego Ninguno o un color del selector de color, que ofrece
un cuadrado de saturación y valor junto a una franja de tonos, una rueda o
deslizadores, y los valores y la opacidad del color; luego el grosor del
contorno, sus uniones, en inglete, redondas o biseladas, sus remates, planos,
redondos o cuadrados, sus guiones, continuo, discontinuo, punteado o de raya y
punto, las puntas de flecha en su inicio y su final — cada pulsación pasa por
ninguna, triángulo, flecha, círculo y rombo —, la regla de relleno, no cero o
par-impar, y la opacidad de cada objeto seleccionado. Capas lista las capas,
la de arriba primero: pulse una para dibujar en ella, y su botón Visible para
mostrarla u ocultarla; sobre la lista, Nueva añade una capa, Quitar quita la
capa en mano, Bloquear la protege de cambios, y Subir y Bajar la mueven.
Objeto coloca la selección por su izquierda y su parte superior y la escala a
un ancho y un alto, la alinea — por los bordes izquierdos, los centros o los
bordes derechos, o por arriba, en medio o abajo — y reparte tres o más objetos
por igual en horizontal o en vertical. Los objetos se alinean dentro de su
selección; un solo objeto, o cualquier número con Alinear a la página
activado, dentro de la página. Forma contiene las esquinas de un rectángulo,
las puntas y el radio interior de una estrella y si es un polígono, y la
familia del texto — sin serifa, con serifa o monoespaciada —, su tamaño,
negrita, cursiva y alineación sobre su origen. Una pestaña más alta que la
ventana se desplaza. Un color que se está eligiendo, un grosor tecleado o
ajustado paso a paso y una opacidad se muestran en el lienzo mientras cambian
y pasan a ser un solo cambio que deshacer cuando se asientan; Tab pasa al
campo siguiente y Mayús+Tab al anterior.

En Estilo, Lineal y Radial pintan el relleno o el contorno con un degradado
ajustado a cada objeto, de su color al blanco, o al negro donde es claro. La
barra de debajo muestra el degradado sobre un damero, con un tirador para cada
parada: pulse un tirador para tomar esa parada en mano —su color pasa entonces
al selector de color— y arrástrelo por la barra; pulse la barra para añadir
allí una parada. Supr, o Quitar parada, quita la parada en mano; Izquierda y
Derecha toman la parada de al lado, y con Mayús la mueven una centésima.
Parada en sitúa la parada en mano, Ángulo gira un degradado lineal, Rellenar,
Reflejar y Repetir dicen cómo sigue un degradado más allá de sus extremos, e
Invertir lo recorre al revés. Bajo el selector, las muestras —grises, luego
doce tonos en color pleno, claro y oscuro, luego las del propio dibujo— dan su
color al color o a la parada en mano; Añadir muestra guarda el color en mano
con el dibujo y Quitar muestra suelta una de las suyas. En Objeto, Rotación
gira la selección en sentido antihorario: un objeto hasta el ángulo dado,
varios juntos según él.

Página fija el ancho y el alto de la página en su unidad, y si queda en
vertical o en horizontal; la unidad — px, pt, pc, mm, cm o in; el espaciado de
la cuadrícula en esa unidad, y en cuántas partes se divide cada lado de sus
cuadrados; ppp de copia, la resolución a la que se hacen sus copias PNG, JPEG
y TIFF; y el fondo de la página, Ninguno o un color de un selector de color
propio, que se muestra en el lienzo mientras se elige. Una página sin fondo se
ve como papel blanco, y una copia suya en PNG es transparente allí donde no
hay nada dibujado.

Un arrastre desde una regla pone una guía horizontal o vertical en el lienzo;
arrastre una guía para moverla, o devuélvala a una regla para quitarla. Ver ▸
Cuadrícula muestra la cuadrícula de la página. Mientras Ajuste está activo, el
puntero se ajusta al dibujar una forma, poner un nodo con la pluma o mover o
escalar la selección: a las guías, a los bordes y el centro de la página, y a
la cuadrícula mientras se muestra. Ver ▸ Contornos muestra cada objeto como su
contorno, lo que hace legible un dibujo recargado. Configurar página, en el
menú Página, muestra la pestaña Página, y el menú fija el papel de la página —
A4, A3, A5, A6, A2, A1, A0, B5, B4, US Letter, US Legal, Tabloide, Ejecutivo,
dos tamaños de pantalla y un icono —, la gira entre vertical y horizontal, le
da el fondo de la pestaña Página o ninguno, y fija sus unidades: píxeles,
puntos, picas, milímetros, centímetros o pulgadas.

Objeto ▸ Agrupar reúne la selección en un grupo y Desagrupar lo deshace;
Traer al frente, Subir, Bajar y Enviar al fondo reordenan los objetos dentro
de su capa; Voltear y Girar reflejan la selección o la giran un cuarto de
vuelta. Bloquear y Ocultar ponen objetos fuera de alcance, y Desbloquear todo
y Mostrar todo en la capa devuelven los de la capa en mano. El menú Trazado
une las formas seleccionadas en una (Unión), recorta las de encima de la más
baja (Diferencia), conserva donde se solapan (Intersección) o donde no
(Exclusión), convierte formas y texto en trazados cuyos nodos se pueden
editar, y convierte contornos en formas rellenas. Estas operaciones se
calculan aparte de la ventana, que sigue libre mientras tanto; un dibujo
cambiado antes de que llegue el resultado conserva su cambio, y el resultado
no se aplica.

Objeto ▸ Recortar con el objeto superior muestra el resto de la selección solo
dentro de su objeto más alto, y Liberar recorte muestra de nuevo entero un
grupo recortado, con el objeto que lo recortaba otra vez encima de lo que
contenía. Trazado ▸ Dividir corta las formas seleccionadas en los trozos que
forman sus solapamientos. Expandir y Contraer agrandan o encogen cada forma
seleccionada según el Desplazamiento de la pestaña Forma, Simplificar vuelve a
trazar cada una con tan pocos nodos como la mantengan dentro de la Suavidad de
la pestaña, e Invertir dirección recorre un trazado al revés, de modo que sus
puntas de flecha y sus guiones lo siguen.

Cada cambio es un paso que deshacer, y Deshacer y Rehacer nombran el paso que
dan; se guardan hasta 1000 pasos, menos cuando falta memoria. Copiar pone la
selección en el portapapeles como SVG, y Pegar añade lo que contiene el
portapapeles a la capa en mano, en su lugar de la página, seleccionado.

Archivo ▸ Colocar imagen (`Ctrl+Shift+P`) pide una imagen — un archivo PNG,
JPEG, GIF, BMP, TIFF, WebP, de icono de Windows u OpenRaster, o un archivo de
sprites de RISC OS, la primera página o el primer sprite de uno que guarde
varios — y la deja en la capa en mano como un solo paso, seleccionada y con el
nombre de su archivo, en medio de lo que muestra el lienzo: a su propio
tamaño, o reducida a cuatro quintos del lienzo donde es mayor. El archivo solo
se lee, nunca se cambia. Una imagen se mueve, se escala, se gira, se recorta y
se copia como cualquier objeto; no tiene estilo ni nodos propios, y donde se
unen, recortan o dividen formas participa como su rectángulo. Se guarda con el
dibujo: SVG la incrusta, una copia PDF o EPS la dibuja y un archivo Draw la
guarda como sprite o como JPEG. Las imágenes de un dibujo suman como mucho 268
millones de píxeles, cada una contada una vez por mucho que se muestre; una
imagen que las llevaría más allá se rechaza, y la banda de estado dice por
qué.

Ajustes, en el menú de la barra de iconos, abre la ventana de ajustes de Draw:
la herramienta con la que empieza una ventana nueva, y si muestra la
cuadrícula, se ajusta y dibuja solo contornos; el papel, la orientación, las
unidades y la cuadrícula con que empieza un dibujo nuevo; el relleno y el
trazo con que se dibujan los objetos nuevos, cada uno ninguno o un color, y el
grosor del trazo; cuánto desplazan las teclas de flecha, un píxel si no se
ajusta; a qué distancia queda un duplicado de lo que copia; y cuánto debe
acercarse el puntero a un tirador, un nodo o una guía para tomarlo. Un cambio
se aplica al momento y se guarda para la próxima vez; Restablecer valores
predeterminados los devuelve todos.

Draw no tiene ninguna capacidad sobre el sistema de archivos. Solo edita el
archivo que se le entregó: un archivo que el usuario puede cambiar se entrega
con escritura y Guardar lo vuelve a escribir, y cualquier otro se abre solo
para lectura y Guardar pregunta dónde guardarlo. Los documentos, los dibujos
pegados desde el portapapeles y las imágenes colocadas se leen en un proceso
de trabajo aparte sin ningún alcance, de modo que un archivo hostil no puede
tocar nada de lo que Draw alcanza.

Pulsar el botón secundario (derecho) del ratón en cualquier lugar de la
ventana abre su menú: Archivo, Página, Edición, Objeto, Trazado, Capa, Ver y
Herramientas, cada uno un submenú. La ventana no tiene barra de menús.
Cerrar una ventana, o salir, con cambios sin guardar pregunta primero. Cerrar
una ventana con el botón secundario sobre su botón de cierre también muestra
su archivo en el gestor de archivos, en la carpeta que lo contiene.

* `Ctrl+N` — una ventana nueva; `Ctrl+O` — abrir un archivo; `Ctrl+Shift+P` — colocar una imagen
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como; `Ctrl+Shift+E` — guardar una copia
* `Ctrl+W` — cerrar la ventana
* `Ctrl+Z` — deshacer; `Ctrl+Shift+Z` o `Ctrl+Y` — rehacer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, pegar; `Ctrl+D` — duplicar
* `Delete` o `Backspace` — borrar la selección
* `Ctrl+A` — seleccionar todo; `Ctrl+Shift+A` — no seleccionar nada; `Escape` — no seleccionar nada, o descartar el trazado en curso
* `Ctrl+G` — agrupar; `Ctrl+Shift+G` — desagrupar
* `Page Up` / `Page Down` — subir o bajar; `Home` / `End` — traer al frente o enviar al fondo
* `Ctrl+[` / `Ctrl+]` — girar a la izquierda o a la derecha
* `Ctrl+L` — bloquear; `Ctrl+Shift+L` — desbloquear todo en la capa
* `Ctrl+H` — ocultar; `Ctrl+Shift+H` — mostrar todo en la capa
* `Ctrl+Shift+C` — objeto a trazado
* `Ctrl+Shift+N` — una capa nueva; `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — subir o bajar la capa
* `Ctrl+=` o `Ctrl++` / `Ctrl+-` — acercar o alejar; `Ctrl+1` — tamaño real; `Ctrl+0` — ajustar la página
* `Ctrl+'` — mostrar u ocultar la cuadrícula; `%` — ajuste sí o no
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `G`, `I`, `Z`, `H` — las herramientas, en orden
* teclas de flecha — mover la selección lo que digan los ajustes, un píxel si no se ajusta; con `Shift`, diez veces más
* `Space` — pulsado, arrastrar el lienzo con cualquier herramienta
* `Ctrl` + rueda — zoom alrededor del puntero; `Shift` + rueda — desplazar en horizontal

## OPTIONS

`-h`, `-?`, `--help`
: Escribe esta ayuda en la salida estándar y termina.

## EXIT STATUS

Cero tras Salir. Distinto de cero cuando se rechazó el canal de ventana, el
buzón de eventos o la sesión del escritorio; el motivo se indica en la salida
de error estándar.
