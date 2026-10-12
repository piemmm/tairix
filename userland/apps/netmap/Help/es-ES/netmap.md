## NAME

netmap — mapa e inventario de la red

## SYNOPSIS

`netmap`

## DESCRIPTION

Abre una ventana que traza un mapa de las redes a las que está conectada esta
máquina: sus interfaces de red, las subredes en las que están, los hosts que
su caché de vecinos ha visto en cada una, los routers por los que pasan sus
rutas y la puerta de enlace predeterminada que lleva más allá de la red local.
Cada elemento se dibuja como un icono sobre un disco del color de lo que es
—ordenadores en azul, routers en naranja, impresoras en morado, almacenamiento
en verde azulado, teléfonos en verde—, y cada línea toma el color de aquello a
lo que lleva. Un host cuyo sistema operativo reconoce el mapa lleva su
emblema, y una luz junto a un host o una interfaz muestra si estaba accesible
la última vez que se vio (verde) o no (rojo).

Arrastre con el botón principal, gire la rueda o desplace con dos dedos en un
panel táctil para moverse por el mapa. Mantenga `Ctrl` y gire la rueda, o
pellizque en un panel o pantalla táctil, para ampliar alrededor del punto bajo
el puntero; pulse `Home` para que todo el mapa vuelva a caber en la ventana.
Haga clic en un nodo para ver lo que se sabe de él —sus direcciones, su
dirección de hardware y su fabricante, su accesibilidad, el tipo de
dispositivo que parece ser y lo que ejecuta, con lo seguro que está el mapa y
por qué— y haga clic en un espacio vacío para ocultar los detalles. Los
nombres aparecen allí donde caben.

Un host que anuncia servicios muestra cuántos en una pequeña insignia. Deje el
puntero sobre un nodo para ver su nombre completo, y sobre un host así para
ver sus servicios nombrados; haga clic en el host para desplegar sus servicios
a su alrededor, y vuelva a hacer clic en él, haga clic en otro sitio o pulse
`Escape` para recogerlos. Haga clic en un nodo con el botón secundario para
abrir su menú: mostrar u ocultar sus servicios, o copiar su dirección, su
nombre o su dirección de hardware —en un servicio, su dirección y puerto, y en
un servicio web, su URL—.

El mapa se lee a través de la API de información del sistema y del servicio de
descubrimiento, y sus hosts reciben el nombre que les dan los servidores DNS
configurados o el que los dispositivos se dan a sí mismos; nunca contacta por
sí mismo con un host, y se vuelve a leer cada pocos segundos mientras su
ventana está abierta. También reúne los servicios que los dispositivos
anuncian en el enlace local —impresoras, recursos compartidos, reproductores
multimedia— y guarda cada uno con el host que lo ofrece. Las direcciones de
interfaz y las rutas necesitan `CAP_SYSINFO_GLOBAL`; las direcciones de
hardware y la caché de vecinos necesitan `CAP_SYSINFO_HW`; los servicios
anunciados necesitan `CAP_NET_DISCOVER_ALL`; nombrar hosts por DNS necesita
`CAP_NET`. Sin alguna, el mapa indica lo que no puede mostrar y dibuja el
resto.

Netmap se inicia desde la Biblioteca de programas del escritorio o por su
nombre desde un shell, y necesita una sesión gráfica en marcha; sin ella,
informa del rechazo en el flujo de error estándar y termina.

## EXIT STATUS

Cero tras Salir; distinto de cero cuando se rechazó el canal de ventana o la
región de imagen compartida (el motivo se indica en el flujo de error estándar).
