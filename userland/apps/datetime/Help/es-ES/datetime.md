## NAME

datetime — fecha, hora y zona horaria de la máquina

## SYNOPSIS

`datetime`

## DESCRIPTION

Abre la ventana **Date & Time** (Fecha y hora): lo que marca el reloj de
la máquina, de dónde procede esa hora y los controles que la ajustan. Se
llega a ella desde el propio menú del reloj del escritorio: haga clic con
el botón derecho en el reloj de la barra de iconos y elija **Set Date &
Time…** (Ajustar fecha y hora…). Antes de que se abra, el escritorio
pide la contraseña de una cuenta que pueda cambiar estos ajustes.

La parte superior de la ventana muestra la hora al segundo, la fecha y
la zona horaria con su abreviatura y su diferencia respecto a UTC, todo
ello en la zona propia de la máquina.

**Date and Time** (Fecha y hora) contiene el reloj propiamente dicho.

- **Set date and time automatically** (Ajustar fecha y hora
  automáticamente) mantiene el reloj en hora mediante los servidores de
  hora de la red. Debajo, la ventana indica de dónde procede la hora del
  reloj: ajustada desde la red, y cuándo; ajustada a mano; leída del chip
  de reloj de la placa al arrancar la máquina; o sin ajustar en absoluto.
  Activar o desactivar el interruptor surte efecto al momento.
- Mientras el interruptor está activado, el calendario y la hora se
  muestran, pero no se pueden cambiar. Desactívelo para ajustar el reloj
  a mano.
- Elija un día en el calendario y escriba la hora o cámbiela paso a
  paso; después pulse **Set**. Las teclas de flecha mueven el día elegido
  un día o una semana, `Page Up` y `Page Down` lo mueven un mes, y con
  `Shift`, un año; los botones junto al mes y al año recorren los meses y
  los años sin cambiar el día. En el campo de la hora, `Tab` o `:` pasa
  de las horas a los minutos y de ahí a los segundos, y `Up` y `Down`
  cambian paso a paso la parte en la que está el cursor. Con el formato
  de doce horas, la última parte es **AM** o **PM**: haga clic en ella o
  pulse `A` o `P`. `Enter` ajusta el reloj.
- Hasta que elija un día o cambie la hora, el calendario y la hora siguen
  al reloj en marcha. Una vez que lo haga, conservan lo que eligió hasta
  que ajuste el reloj o cierre la ventana.

La fecha y la hora que ajuste se interpretan en la zona horaria de la
máquina. Una hora que los relojes se saltan al adelantarse se rechaza, y
la ventana indica cuándo cambian los relojes. Si se trata de una hora
por la que los relojes pasan dos veces al atrasarse, la ventana
pregunta: elija si se refiere a la de antes del cambio o a la de
después. Una fecha anterior a la creación de este sistema, o posterior a
ella en más de un siglo, solo se ajusta después de que usted la confirme,
porque los certificados no se considerarían de confianza y los archivos
parecerían más antiguos, o más recientes, de lo que son.

Tras ajustar el reloj, el chip de reloj de la placa se pone a la misma
hora, para que la máquina la conserve al reiniciarse. Si la máquina no
tiene chip de reloj, la ventana avisa de que la hora se perderá en el
próximo reinicio, a menos que la hora de red esté activada.

**Time Zone** (Zona horaria) nombra la zona que sigue la máquina, indica
cuándo se adelantarán o atrasarán sus relojes la próxima vez y cuánto, y
ofrece el botón **Change…**, que abre una lista de todos los lugares que
conoce el sistema: escriba parte del nombre de una ciudad, una región o
una zona para acotarla y elija después con `Enter` o con un doble clic.
Cada lugar muestra la hora que es allí en ese momento. Una zona nueva
surte efecto al momento.

La zona de la máquina es la que muestra la pantalla de inicio de sesión
y la que usa toda cuenta que no haya elegido una zona propia en
**Settings** (Ajustes).

Si la cuenta con la que se ejecuta la ventana no puede hacer un cambio,
este se rechaza, un aviso explica el motivo y la ventana muestra lo que
realmente está en vigor. `Escape` o **Close** cierra la ventana.

## EXIT STATUS

Cero cuando se cierra la ventana, incluso después de un cambio
rechazado. Distinto de cero cuando no se pudo abrir la ventana o se
perdió su conexión con el escritorio; el motivo se indica en el flujo de
error estándar.

## SEE ALSO

`configure`
