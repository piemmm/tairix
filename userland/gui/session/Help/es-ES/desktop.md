## NAME

desktop — iniciar la sesión gráfica de escritorio

## SYNOPSIS

`desktop`

## DESCRIPTION

Inicia la sesión gráfica de escritorio en el puesto de esta máquina: el
comando adquiere el arrendamiento exclusivo de pantalla y entrada del
puesto, se conecta al servicio de pantalla y ejecuta el escritorio
compositado — el gestor de ventanas y la barra de tareas — hasta que la
sesión termina. El comando retorna cuando la sesión de escritorio
termina.

El mismo escritorio se inicia automáticamente tras la autenticación: un
inicio de sesión gráfico (`os.loginType`) es el valor por omisión en una
máquina que pueda ejecutarlo. Este comando lo inicia bajo demanda desde
un shell de texto.

Cuando no hay ningún servicio de pantalla en ejecución, o cuando otra
sesión ya posee el puesto, el comando falla escribiendo su motivo en la
salida de error estándar — nunca desplaza una sesión en curso.

Mientras el escritorio está en marcha, `Meta+Shift+3` guarda una imagen
de la pantalla y `Meta+Shift+4`, una de la zona que arrastre o, tras
`Space`, de la ventana en la que haga clic. `Meta+Shift+5` abre la barra
de captura, que también graba la pantalla. Si añade `Ctrl`, la imagen va
al portapapeles, y un atajo de captura pulsado durante una grabación la
detiene. Una grabación con sonido pide la contraseña de un administrador
cuando su cuenta no puede grabar sonido por sí misma.

## OPTIONS

- `-h, -?` — mostrar la ayuda corta de este comando.

## EXAMPLES

- `desktop` — iniciar la sesión de escritorio.

## EXIT STATUS

- `0` — se sirvió la ayuda corta.
- `1` — la línea de comandos no se entendió.
- cualquier otro código distinto de cero — la sesión no pudo iniciarse
  (sin puesto, sin servicio de pantalla) o terminó (se perdió el
  arrendamiento del puesto); el motivo se escribe en la salida de error
  estándar.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta (una
  etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `configure`
- `man`
