## NAME

recorder — grabar la pantalla para el escritorio

## SYNOPSIS

`recorder SESSION`

## DESCRIPTION

Graba la pantalla en una película mientras el escritorio se lo pida. No se
ejecuta a mano: el escritorio lo inicia cuando empieza una grabación, y
termina cuando termina la grabación.

El programa que lo inicia indica como `SESSION` la sesión del escritorio
para la que graba.

Para grabar, pulse `Meta+Shift+5` para abrir la barra de captura, elija la
pantalla entera, una ventana o una zona, y pulse **Record** (Grabar). Un
atajo de captura, el indicador de grabación del área de notificaciones o la
duración elegida en **Options** (Opciones) la detiene.

Una grabación se guarda como `Screen Recording YYYY-MM-DD at HH.MM.SS.webm`
en la carpeta Desktop o en `Home/Videos`, según se elija en **Options**. Es
una película WebM: su imagen es vídeo VP8 de hasta treinta fotogramas por
segundo, tomados solo cuando algo cambia en la pantalla, y su sonido es
Opus.

**Sound** (Sonido) en **Options** elige lo que oye la grabación: nada, el
micrófono, el sonido propio del escritorio o ambos mezclados. La grabación
con sonido de un administrador empieza enseguida. La de cualquier otra
cuenta pide antes la contraseña de un administrador, y entonces el grabador
se ejecuta como ese administrador para esa única grabación; si se cancela,
no se graba nada, y un aviso lo indica.

Cuando el disco se llena, la grabación termina ahí y se conserva. Cuando el
grabador falla, lo que grabó se conserva y un aviso indica qué ocurrió.

## EXIT STATUS

Cero cuando la grabación terminó, incluida una que se detuvo al llenarse el
disco. Distinto de cero cuando no pudo empezar o terminó de cualquier otro
modo; el motivo se escribe en la salida de error estándar.

## SEE ALSO

`desktop`
