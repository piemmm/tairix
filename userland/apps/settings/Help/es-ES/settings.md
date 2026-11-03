## NAME

settings — configurar el escritorio y esta máquina

## SYNOPSIS

`settings`

## DESCRIPTION

Abre una ventana de escritorio que enumera cada categoría de ajustes de este
sistema: qué es la máquina, el aspecto del escritorio, la pantalla, la red, los
dispositivos con los que se maneja, las cuentas que lo usan y los volúmenes que
contiene. Elegir una categoría en la barra lateral muestra su panel. Una
categoría que contiene varios paneles abre y cierra en su lugar la lista de
ellos, y puede haber a la vez tantas de esas listas abiertas como se quiera.

Settings no posee autoridad propia. Cada cambio es o una petición a la sesión
de escritorio, dueña de los ajustes del usuario, o una ejecución
reautenticada del comando que ya escribe ese almacén; nada de aquí puede
elevar un privilegio.

Una categoría que este sistema no puede atender lo dice con claridad y nombra
lo que tendría que existir para poder hacerlo. Nunca se muestra un control que
no cambiaría nada.

Escriba en el campo de búsqueda sobre la barra lateral para filtrarla a las
categorías y ajustes que alcanza una palabra. `Tab` y `Shift+Tab` mueven el
foco entre el campo de búsqueda, la ruta, la barra lateral y el panel; `Up` y
`Down` recorren la barra lateral y `Enter` abre la fila. `Right` y `Left` abren
y cierran la lista de la categoría bajo el cursor. Una ventana demasiado
estrecha descarta la barra lateral, y la primera miga de la ruta enumera
entonces las categorías.

Se lanza desde la fila *Settings…* del menú de sistema del escritorio, o desde
un intérprete de órdenes por su ruta, `/System/Components/settings.app`.
Requiere una sesión gráfica en marcha: sin ella el canal de ventana es
inalcanzable y comunica el rechazo en el flujo de error estándar y termina.

Sonido tiene dos paneles. *Output & Input* elige el dispositivo por el que
suena el sonido y aquel desde el que se graba, y ajusta el volumen y el
silencio de ese dispositivo: silenciarlo baja el volumen a nada hasta que quita
el silencio, que lo devuelve a donde estaba. También muestra lo que está
grabando: sus propias grabaciones por su nombre y cuántas hacen los demás. Esto
cambia el dispositivo al instante, y solo mientras su sesión ocupa la sala a la
que sirve el dispositivo; un dispositivo de la sala de otra sesión se muestra
pero no se puede cambiar aquí. Su escritorio recuerda lo que ajusta. *Sound
Effects* activa o desactiva los sonidos propios del escritorio, fija su tema y
su volumen y, para cada evento —una notificación, un error, la campana del
terminal—, si reproduce el sonido de su tema, un archivo que usted elija o
nada. Los sonidos de arranque, inicio y cierre de sesión son del equipo y los
fija su administrador.

**Raspberry Pi** solo aparece cuando la máquina lo es. Ajusta el ventilador del
procesador, conmutado a través de una línea del conector de 40 pines: *Off*,
*On* todo el tiempo o *Automatic*, que enciende el ventilador cuando el
procesador alcanza la temperatura elegida y no lo apaga hasta que se ha enfriado
el margen elegido, para que no se encienda y se apague en torno a un mismo
grado. La línea de control se elige por su número GPIO y su pin, junto con si el
ventilador funciona cuando la línea está a nivel alto o bajo; una línea que ya
usa otra cosa se nombra en lugar de ofrecerse. Una línea del conector no puede
alimentar un ventilador por sí sola, así que conéctelo mediante un transistor.
El panel muestra también la temperatura del procesador y si el ventilador está
en marcha. Cuando *Automatic* no puede leer la temperatura, el ventilador
funciona hasta que pueda. Como los demás ajustes de la máquina, un cambio espera
a *Apply*, que pide una cuenta que pueda cambiarlos.

**Storage** enumera cada volumen montado, salvo el que otro usuario
mantiene privado: dónde está montado, su sistema de archivos, su dispositivo y
su soporte, lo lleno que está y si está en buen estado. Es un informe, no un
control —montar y desmontar corresponden al gestor de archivos y al comando
`mount`— y un volumen cuyo formato no guarda ninguna capacidad fija lo dice en
lugar de mostrar una barra. `df` informa de las mismas cifras en un intérprete
de órdenes.

**Files** establece cómo funciona el gestor de archivos: dónde se abre una
ventana nueva y cómo muestra y ordena lo que enumera, si muestra la columna de
lugares y la barra de herramientas, si las carpetas quedan arriba, qué sufijos
de nombre y qué elementos se ocultan, si una imagen muestra su propio contenido
y una carpeta lo que contiene, si mover elementos a la Papelera pide
confirmación antes y si una imagen de disco o un archivo comprimido que usted
abrió se desmonta una vez cerradas sus ventanas. Una fila surte efecto en cuanto
se elige: dónde se abre una ventana nueva y si muestra la columna de lugares y
la barra de herramientas se aplican a la próxima ventana que usted abra, y todo
lo demás llega a todas las ventanas del gestor de archivos a la vez, aunque una
ventana conserva una vista o un orden que usted mismo haya elegido en ella.
*Settings…* en el propio menú del gestor de archivos abre este panel.

## EXIT STATUS

Cero tras un cierre limpio; distinto de cero cuando se rechazó el canal de
ventana o la región de fotogramas compartida (el motivo se indica en el flujo
de error estándar).
