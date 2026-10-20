## NAME

patience — Klondike, FreeCell y Spider, con un solucionador detrás de cada pista

## SYNOPSIS

`patience`

## DESCRIPTION

Abre una ventana del escritorio dispuesta para una partida de paciencia, los
juegos de cartas que también se llaman solitarios. Hay seis juegos en el menú
Game, y las teclas `1` a `6` los eligen: el Klondike robando una carta o tres,
el FreeCell y el Spider con uno, dos o cuatro palos.

En el Klondike, construya las siete columnas en orden descendente y alternando
colores, y lleve cada palo a su fundación, del as al rey. Haga clic en el mazo
para pasar cartas al descarte; cuando se vacíe, haga clic en su lugar para
volver a dar la vuelta al descarte. En el FreeCell todas las cartas se reparten
boca arriba y cada una de las cuatro celdas libres guarda una carta; una
escalera se mueve de una vez a través de las celdas libres y las columnas vacías
mientras tengan sitio para ella. En el Spider, construya en orden descendente
con cualquier palo, pero mueva una escalera solo si es toda de un palo; una
escalera completa del rey al as va sola a las fundaciones. Haga clic en el mazo
para repartir una carta en cada columna, lo que exige antes que cada columna
tenga una.

Arrastre las cartas adonde quiera, o haga clic en ellas: un clic envía las
cartas al mejor lugar al que puedan ir, primero a las fundaciones, luego a otra
columna, luego a una columna vacía y luego a una celda libre. Las cartas que no
pueden ir a ninguna parte dan un saltito donde están. Las cartas que ninguna
jugada posterior podría necesitar van solas a las fundaciones, y en cuanto nada
puede impedir la victoria, van todas. Con Send cards home desactivado, Finish
(`F`) las envía.

Cada pista procede de un solucionador que juega la partida por delante de usted,
así que una pista (`H`) es una jugada que mantiene la partida ganable, no solo
una que las reglas permiten. El solucionador examina cada posición a la que
llega. Cuando demuestra que no queda ninguna línea ganadora, la barra de estado
lo indica, y Back to last winnable (`Ctrl+B`) le devuelve a la última posición
desde la que aún se podía ganar.

Con Winnable deals only activado, como al principio, cada reparto nuevo es uno
que el solucionador ya ha ganado, y una marca junto al número del reparto lo
indica. El solucionador abandona un reparto que no logra resolver deprisa, y el
reparto que le da entonces queda marcado como no comprobado; el Spider de cuatro
palos suele ser demasiado profundo para resolverse a tiempo. Cada reparto lleva
su número en la barra de estado, y el menú propio de la mesa, abierto con el
botón secundario, juega un reparto por su número. Los repartos del FreeCell se
numeran como en otros juegos de FreeCell, de modo que un reparto numerado es la
misma partida en todas partes.

El Klondike se puntúa de la forma estándar, o al estilo de Las Vegas, donde cada
partida cuesta 52 $ y cada carta enviada a las fundaciones gana 5 $, con una
sola pasada por el mazo robando una carta y tres robando tres, o al estilo de
Las Vegas acumulado, que arrastra las ganancias de una partida a otra. El Spider
empieza con 500 puntos, pierde uno por cada jugada y gana 100 por cada escalera
enviada a las fundaciones. Para cada juego, las estadísticas guardan las
partidas jugadas y ganadas, la racha actual de victorias o derrotas, la racha de
victorias más larga, la victoria más rápida y la victoria con menos jugadas.
Repartir de nuevo antes de ganar una partida la cuenta como perdida.

La partida en curso se conserva: cerrar la ventana o salir la deja para la
próxima vez, y el reloj se detiene mientras la ventana está oculta. El menú del
juego en la barra de iconos y el menú propio de la mesa reúnen las opciones: los
repartos ganables, la puntuación, el envío de cartas a las fundaciones, los
palos de cuatro colores, los reversos de las cartas y la mesa.

El teclado hace todo lo que hace el puntero. Las teclas de flecha mueven un
anillo de montón en montón y por las cartas de una columna; `Space` recoge las
cartas y las deja donde esté el anillo, `Enter` las envía a su mejor lugar y
`Escape` las devuelve. `D` da la vuelta al mazo, `Ctrl+Z` deshace una jugada y
`Ctrl+Y` la rehace, `N` reparte una partida nueva, `R` vuelve a repartir esta y
`S` muestra las estadísticas.

El juego se inicia desde la biblioteca de programas del escritorio, en Games, o
por su nombre desde un intérprete de órdenes. Necesita una sesión gráfica en
marcha: sin ella el canal de ventana es inalcanzable y el juego comunica el
rechazo en el flujo de error estándar y termina.

## EXIT STATUS

Cero tras una salida limpia; distinto de cero cuando se rechazó el canal de
ventana o la región de marco compartida (el motivo se indica en el flujo de
error estándar).
