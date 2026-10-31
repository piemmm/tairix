## NAME

unmount — desmontar un volumen montado

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Retira del servicio el volumen montado bajo `name`: el sistema de
archivos y el dispositivo se vacían, el montaje bajo `/Storage` se
retira y la raíz duradera `id::` del volumen se revoca. `name` es el
nombre de catálogo del volumen (`usb1`), un directorio en el que está
montado (`/Storage/usb1`, o aquel al que `mount imagen directorio` lo
enlazó), o el nombre de archivo de la imagen de disco desde la que se
montó (`Workbench.adf`), comparado con las listas de montajes y de
vistas de la API de información del sistema; un nombre de archivo que
coincide con el de la imagen de otro usuario designa la de usted. Un
nombre de archivo que podría designar más de un volumen, o del que la
lista solo guarda el principio, no designa ninguno: nombre entonces el
volumen por el lugar donde está montado.
Una ruta puede ser relativa o terminar en una barra: se resuelve como
`mount` resuelve una. El volumen privado de otro usuario no muestra ni
su nombre de archivo ni los directorios a los que está enlazado, así que
se nombra por el lugar donde está montado en `/Storage`.

Un volumen cuyo dispositivo fue retirado con escrituras aún sin
confirmar sigue visible como `unavailable-dirty` (o
`unavailable-lost`) en la lista de montajes, y un `unmount` simple lo
rechaza: sus datos retenidos se conservan para una reinserción
verificada. Un `unmount` simple también rechaza un volumen con
archivos aún abiertos en él o que un programa tiene como directorio de
trabajo. `--force` es la salida deliberada: los archivos aún abiertos
en el volumen quedan desconectados, un programa que trabaja en él se
queda en un directorio que ya no existe, los datos retenidos se
descartan, el volumen se retira y lo que se abandonó queda registrado
en el registro de auditoría. En un volumen sano, `--force` sigue
vaciando y desmontando limpiamente; nada se descarta cuando es posible
una confirmación limpia.

Puede desmontar una imagen de disco o un recurso compartido de red que
haya montado usted mismo; cualquier otro volumen requiere la autoridad
de montaje (`CAP_FS_MOUNT`). El núcleo la comprueba y audita cada
decisión. Los volúmenes de arranque permanentes y los enlaces de vista
del sistema no se pueden desmontar.

## OPTIONS

- `-f, --force` — desmontaje forzado: retirar el volumen aunque haya
  archivos abiertos en él, un programa trabaje en él o sus datos sin
  confirmar no puedan confirmarse, desconectando los archivos abiertos
  y descartando los datos retenidos.
- `-h, -?, --help` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `unmount usb1` — desmontar limpiamente el volumen montado como
  `usb1`.
- `unmount /Storage/usb1` — lo mismo, nombrado por su punto de
  montaje.
- `unmount Workbench.adf` — desmontar la imagen de disco montada desde
  `Workbench.adf`.
- `unmount --force usb1` — retirar un volumen no disponible
  descartando sus datos retenidos sin confirmar.

## EXIT STATUS

- `0` — el volumen se desmontó (o se escribió la ayuda breve).
- `1` — no se entendió la línea de órdenes, o quien llama no puede
  desmontar el volumen.
- `2` — falló el servicio de información del sistema o la escritura en
  el terminal.
- `32` — el volumen no estaba montado o no es desmontable, la ruta que
  lo nombra no pudo resolverse, o su desmontaje falló.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `mount`
- `df`
- `man`
