## NAME

configure — leer y establecer los ajustes de la máquina

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

Lista, muestra y establece los ajustes de la máquina, guardados en
`/System/Settings/Configuration/system.conf`. Sin operandos se lista
cada ajuste con su valor actual; con una clave sola se muestra su valor;
con uno o más pares `<key> <value>` se cambian esos ajustes a la vez.

Varios pares son un solo cambio, no varios: cada par se comprueba antes
de cambiar nada, y después los ajustes se guardan de una sola vez. Una
línea de órdenes que nombre una clave desconocida, un valor fuera del
conjunto de su clave o la misma clave dos veces no cambia nada en
absoluto, de modo que un grupo de ajustes nunca puede quedar aplicado a
medias.

Solo un administrador puede cambiar un ajuste: una cuenta ordinaria lee
los ajustes de la máquina pero no cambia ninguno. El sistema trae sus
propios ajustes, que rigen hasta que un administrador cambia alguno, y
el primer cambio conserva todos los demás ajustes que trae el sistema.
Un cambio surte efecto la próxima vez que arranca la parte del sistema
que lo lee (`os.loginType` en el próximo inicio de sesión, los
conmutadores `cache.*` en el próximo arranque), salvo que su clave, más
abajo, indique otra cosa.

El conjunto de claves es cerrado: una clave desconocida, o un valor
fuera del conjunto de una clave, se rechaza indicando las opciones
válidas y no cambia nada.

- `os.loginType` — `text` o `graphical`: qué tipo de sesión inicia el
  servicio de inicio de sesión para un usuario autenticado. `graphical`
  (el valor por defecto) inicia directamente la sesión de escritorio
  tras la autenticación, y recurre al inicio de sesión de texto en una
  máquina que no puede ejecutar ninguna; `text` inicia el shell de la
  cuenta — el escritorio solo se ejecuta desde un inicio de sesión
  gráfico.
- `cache.all` — `on` u `off`: el conmutador maestro de caché. `on` (el
  valor por defecto) deja que cada clase de caché de abajo siga su
  propio ajuste; `off` es un techo que desactiva toda caché en memoria
  sin importar los ajustes por clase.
- `cache.filesystem`, `cache.block`, `cache.transform`,
  `cache.semantic` — `auto` u `off`: los conmutadores por clase para
  las cuatro cachés de memoria recuperables (las cachés del sistema de
  archivos, del bloque de disco completo, del clúster descomprimido y
  del arranque de aplicaciones). `auto` (el valor por defecto) deja que
  el gestor de presión de memoria gobierne la clase; `off` la desactiva
  por completo. No hay un `on` por clase: no se puede forzar a una clase
  a ignorar la presión de memoria. Una clase está efectivamente `off`
  siempre que `cache.all` esté en `off`.

Cada caché es un acelerador recuperable, nunca la fuente de la verdad,
así que apagar cualquiera o todas ellas solo hace más lento el trabajo
afectado — nunca cambia un resultado.

- `net.ipv4.enabled`, `net.ipv6.enabled` — `true` o `false`: los
  conmutadores de familias de direcciones de toda la pila. Ambos son
  `true` de forma predeterminada. Una familia desactivada no vincula
  direcciones, no responde a paquetes y rechaza un socket de esa
  familia con un error tipado — nunca un descarte silencioso.
- `net.ipv6.privacy` — `true` o `false`: si la pila forma direcciones
  IPv6 temporales (de privacidad) además de la estable. `false` (la
  predeterminada) usa solo la dirección SLAAC estable.
- `net.tcp.syncookies` — `auto` o `always`: la defensa contra
  inundaciones SYN. `auto` (la predeterminada) mantiene una cola
  semiabierta acotada y recurre a cookies sin estado al desbordarse;
  `always` responde a cada solicitud de conexión sin estado. No hay
  `off` — una cola de conexiones indefensa no es una opción.
- `net.tcp.keepalive` — `true` o `false`: si las conexiones TCP envían
  sondas de mantenimiento en un enlace inactivo. `false` (la
  predeterminada) nunca sondea ni cierra una conexión inactiva; `true`
  sondea a un par inactivo tras el intervalo habitual y cierra la
  conexión si deja de responder.
- `net.tcp.ecn` — `true` o `false`: si las conexiones TCP negocian la
  notificación explícita de congestión (ECN). `false` (la
  predeterminada) deja las conexiones Not-ECT; `true` ofrece ECN en el
  saludo y luego trata una marca de congestión como una señal para
  reducir la velocidad en lugar de forzar la pérdida de un paquete.
- `net.sockets.mem` — `auto` o un tamaño en bytes como `64M`: la
  memoria que la pila de red puede mantener en estado de sockets entre
  todos los principales. `auto` (la predeterminada) lo dimensiona según la
  RAM de la máquina, para que un servidor grande no quede atado a una
  cifra elegida en uno pequeño; un tamaño lo sustituye para una carga que
  usted conoce mejor. Cada principal puede tener una dieciseisava parte
  del presupuesto efectivo, así que siempre hay sitio para dieciséis. En
  bytes y no en número de sockets, porque el mismo número de sockets son
  unos kilobytes en reposo y megabytes cuando están llenos: el
  presupuesto lleva muchas conexiones tranquilas o menos conexiones
  activas, según sea la carga en realidad.
- `time.servers` — `none` o una lista de servidores de hora de red
  separada por comas, cada uno un nombre de host o una dirección. `none`
  (el valor por omisión) no nombra ningún servidor propio: el reloj se
  ajusta entonces desde el servidor que indica la concesión de dirección
  de la red, o si no desde el grupo público `pool.ntp.org`. Los servidores
  nombrados aquí tienen prioridad sobre ambos.
- `time.refresh` — `6h`, `12h`, `1d`, `2d` o `7d`: cuánto tiempo de
  actividad pasa entre consultas del reloj una vez conocida la hora. `1d`
  es el valor por omisión. Un reloj sin ajustar, inverosímil o muy
  desactualizado se corrige en cuanto la red lo permite, diga esto lo que
  diga.
- `time.network` — `on` (el valor por omisión) u `off`: si el reloj se
  ajusta desde la red. Con `off` no sale ninguna consulta de hora hacia
  ningún servidor, y el reloj se ajusta a mano o desde el chip de reloj de
  la placa, que se lee en cada arranque en cualquier caso. Un cambio se
  aplica al momento; el ajuste se guarda en el disco del sistema, así que
  al principio de un arranque rige el valor por omisión hasta que se
  alcanza ese disco.
- `time.zone` — `default` o el nombre de una zona horaria como
  `Europe/London`: la zona horaria de la máquina, la que muestra la
  pantalla de inicio de sesión y la que usa una cuenta que no ha elegido
  ninguna. `default` es la zona que el sistema trae como propia; un
  nombre que no figura en la lista de zonas del sistema se pasa por alto
  como si fuera `default`. Un cambio se aplica al momento.
- `input.mouse.debounce` — milisegundos enteros, `25` por omisión, `0` para
  desactivarlo, `100` como máximo: cuánto tiempo después de soltar un botón
  del ratón se ignora la siguiente pulsación del mismo botón como rebote del
  interruptor en lugar de tratarla como un clic nuevo. Un interruptor
  desgastado puede notificar una segunda pulsación unos milisegundos después
  de soltarlo cuando pretendía un solo clic. Ponga `0` en un ratón cuyo modo
  de disparo rápido envía pares de clics a propósito.
- `audio.output`, `audio.input` — `auto` por defecto, o la ubicación de un
  extremo tal como la muestra `audioctl` (dieciséis dígitos hexadecimales,
  un punto y el índice del extremo): la salida y la entrada que esta
  máquina prefiere por defecto. La elección propia de una sesión va
  primero; esta es la elección antes de que nadie inicie sesión y en una
  máquina sin escritorio. `auto` es el primer dispositivo encontrado.
- `audio.level` — en decibelios, `0dB` por defecto, o una atenuación como
  `-12dB` o `-6.5dB`, con dos decimales como máximo: el nivel con el que
  empieza cada salida y entrada. Nunca por encima de `0dB`. El servicio de
  audio toma un valor cambiado en el siguiente arranque; `audioctl` cambia
  la máquina en marcha.
- `cooling.fan.mode` — `off` por defecto, `on` o `auto`: si el ventilador de
  la CPU del conector GPIO de la placa no se acciona, funciona siempre o se
  conmuta según la temperatura del procesador.
- `cooling.fan.line` — `none` por defecto, o la línea GPIO a la que está
  conectado el ventilador, de `0` a `63`. Solo puede accionarse una línea
  que preste el conector de la placa.
- `cooling.fan.signal` — `active-high` por defecto, o `active-low`: qué
  nivel de la línea hace funcionar el ventilador.
- `cooling.fan.start`, `cooling.fan.hysteresis` — grados Celsius enteros,
  `65` y `10` por defecto: un ventilador automático arranca en `start`, de
  `40` a `85`, y se detiene cuando el procesador se ha enfriado `hysteresis`
  grados, de `2` a `20`. Un cambio en un ajuste del ventilador se aplica al
  momento.

Una clave que no está en la lista anterior se busca en un segundo
registro: los ajustes de red de cada interfaz, guardados en
`/System/Settings/Network/network.conf`. Sus claves se escriben
`<interface>.<setting>`, donde `<interface>` es el alias que un
administrador dio a una interfaz de red (`wan`, `lan0`) y `<setting>` es
uno de `kind`, `match.mac`, `match.node`, `ipv4.method`, `ipv4.address`,
`ipv4.gateway`, `ipv6.method`, `ipv6.address`, `ipv6.gateway`,
`dns.servers`, `mtu`, `bond.members`, `bond.mode`,
`bond.monitor-interval` y `bond.primary`.

El registro de la máquina de arriba se consulta siempre primero, así que
un alias de interfaz nunca puede apropiarse del nombre de un ajuste de
la máquina.

Los ajustes de red no tienen valores por defecto, así que la lista
muestra solo los ajustes que están establecidos, y mostrar uno que no lo
está responde con una línea vacía en lugar de inventar un valor. Solo un
administrador puede leerlos o cambiarlos: nombran el hardware de cada
interfaz y las direcciones de esta máquina. La lista de cualquier otra
cuenta muestra los ajustes de la máquina e indica que los de red no se
muestran.

Como ese registro no tiene valores por defecto, un ajuste se elimina en
lugar de restablecerse, y un **valor vacío** es la forma de eliminarlo:
`configure wan.ipv4.address ""`. Ningún ajuste de interfaz acepta un
valor vacío, así que esta escritura nunca puede confundirse con
establecer uno. Esto es lo que permite que una sola orden pase una
interfaz de una dirección estática a DHCP —
`configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
— un cambio del que ninguna de las dos mitades describe por sí sola una
configuración coherente. Una interfaz cuyo último ajuste se elimina deja
de estar declarada en absoluto.

Toda la configuración editada se comprueba antes de guardar nada: un
cambio que la dejaría incoherente (un método estático sin dirección, un
agregado de enlaces con menos de dos miembros) se rechaza y no se guarda
nada.

Cambiar un ajuste `net.*`, o uno de interfaz, lo guarda y lo entrega a
la pila de red en marcha, así que surte efecto al momento. Solo se
entregan las interfaces a las que el cambio afectó de verdad. Si la pila
en marcha no acepta una entrega — no hay ninguna en marcha, o su cuenta
no puede administrar la red —, el ajuste se guarda igualmente y
`configure` lo indica; entonces se aplica en el próximo arranque.

Dos límites se comunican en lugar de ocultarse. Una interfaz que usted
quitó de la configuración sigue funcionando hasta el próximo arranque
con la última configuración que recibió la pila, porque la pila no tiene
ningún mensaje que retire una. Una interfaz guardada sin `match.mac` ni
`match.node` nunca puede vincularse a un dispositivo, así que se guarda
y se indica el rechazo.

## OPTIONS

- `-h, -?` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `configure` — listar todos los ajustes.
- `configure os.loginType` — mostrar el tipo de sesión por defecto.
- `configure os.loginType graphical` — arrancar en el inicio de sesión
  gráfico.
- `configure cache.all off` — desactivar toda caché en memoria en todo
  el sistema.
- `configure cache.filesystem off` — desactivar solo la caché del
  sistema de archivos.
- `configure net.ipv6.enabled false` — desactivar IPv6 en toda la pila.
- `configure time.servers 0.example.test,1.example.test` — establecer
  los servidores de hora de red con los que se sincroniza el reloj.
- `configure time.zone Europe/London` — hacer que la máquina siga la
  hora de Londres.
- `configure wan.ipv4.address` — mostrar la dirección IPv4 estática
  configurada para la interfaz llamada `wan`.
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  dar a `wan` una dirección IPv4 estática y una puerta de enlace
  predeterminada.
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — pasar `wan` del direccionamiento estático a DHCP.
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — establecer los
  servidores DNS recursivos que se usarán en `wan`.

## EXIT STATUS

- `0` — se completó la lista, el valor, la ayuda breve o el cambio.
- `1` — los ajustes no se pudieron leer o cambiar (por ejemplo, quien
  llama no puede cambiar los ajustes de la máquina, o no puede leer sus
  ajustes de red), la clave es desconocida, el valor está fuera del
  conjunto de la clave, el cambio dejaría incoherente la configuración
  de red, o la salida no se pudo entregar. Una línea de órdenes no
  entendida también termina con `1`.

## ENVIRONMENT

- `LANG` — el idioma preferido de la ayuda breve (una etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `man`
