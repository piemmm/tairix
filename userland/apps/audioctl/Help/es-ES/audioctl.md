## NAME

audioctl — listar los dispositivos de sonido y cambiar sus controles

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

Lista las salidas y las entradas que ve esta sesión: el identificador de cada
una para este arranque, si es la predeterminada de su sentido, su nivel y su
silencio, la frecuencia a la que funciona, las tramas perdidas, su ubicación y
su nombre. `streams` lista sus propios flujos, y con `--all` los de todos los
principales.

`default`, `level`, `mute` y `unmute` cambian los controles de un
dispositivo. Un dispositivo se nombra con una referencia `audio:`:
`audio:sink/default` o `audio:source/default` para el predeterminado ahora,
`audio:sink/<id>` para este arranque, o `audio:sink/<location>` esté donde
esté el dispositivo, tal como los nombra la lista. Un nivel se expresa en
decibelios a la centésima, 0 o menos, como `-6` o `-3.5`; un nivel negativo no
necesita `--`.

Los controles de un dispositivo pertenecen a la sala a la que sirve. La sesión
que ocupa esa sala puede cambiarlos, cualquiera puede mientras la sala está
libre, y nadie mientras está retenida; una negativa lo indica. Lo que una
sesión ajusta es suyo: mientras otra sesión ocupa la sala se aparta, y vuelve
cuando regresa. El nivel con el que empieza cada dispositivo y los
dispositivos preferidos como predeterminados son los ajustes de la máquina
`audio.level`, `audio.output` y `audio.input`, que establece `configure`.

En la información estándar (fd 3), `audioctl streams` escribe un registro
`omission` cuando lista solo sus propios flujos.

## OPTIONS

- `-a, --all` — con `streams`, los flujos de todos los principales; requiere `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — mostrar la ayuda breve propia de esta orden.
- `--version` — mostrar la versión y salir.

## EXAMPLES

- `audioctl` — listar las salidas y las entradas.
- `audioctl level audio:sink/default -10` — poner la salida predeterminada diez decibelios por debajo del máximo.
- `audioctl mute audio:source/default` — silenciar la entrada predeterminada.
- `audioctl default audio:sink/2` — hacer predeterminada la salida 2.
- `audioctl streams --all` — listar los flujos de todos los principales.

## EXIT STATUS

- `0` — la orden se completó.
- `1` — se rechazó, o no pudo llevarse a cabo.
- `2` — no se entendió la línea de órdenes.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve (una etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
