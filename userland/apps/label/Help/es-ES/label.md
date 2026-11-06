## NAME

label — mostrar o cambiar el título de un volumen

## SYNOPSIS

`label [option...] name [title]`

## DESCRIPTION

Muestra el título del volumen que designa `name` o, si se da `title`,
lo establece. El título de un volumen es la etiqueta que registra su
sistema de archivos: el nombre con el que se muestra y el que recibe en
`/Storage` la próxima vez que se monta. `name` es el nombre de catálogo
del volumen (`usb1`), un directorio donde está montado
(`/Storage/usb1`), el nombre de archivo de la imagen de disco desde la
que se montó (`Workbench.adf`) o su raíz duradera (`id::…`), nombrados
como en `unmount`. Un `title` vacío borra el título donde el formato
del volumen lo permite.

Mientras el volumen siga montado, su nombre en `/Storage` no cambia,
así que nada que lo nombre allí deja de funcionar; el nuevo título lo
nombra a partir de su próximo montaje. Su identidad y la de sus
archivos siguen siendo las mismas.

Cada formato escribe un título a su manera, y un título que no puede
guardar se rechaza en lugar de acortarse. Un volumen FAT admite hasta
once letras mayúsculas, dígitos, espacios y la puntuación que permite
un nombre de archivo corto, sin empezar ni terminar con un espacio; un
volumen ext2, ext3 o ext4 hasta dieciséis bytes; un volumen ADFS hasta
diez caracteres; un volumen AmigaDOS de uno a treinta; un volumen UDF
hasta 126; y un volumen ARXFS hasta 252 bytes, salvo que esté cifrado.
Un disco masterizado con título, un archivo comprimido y un recurso
compartido de red no admiten uno nuevo.

Una imagen de disco que haya montado usted mismo es suya para cambiarle
el título. Un disco extraíble —una memoria USB, una tarjeta— es suyo
para cambiarle el título mientras usa el propio ordenador, si puede
cambiar lo que hay en su carpeta superior; cualquier otro volumen
requiere la autoridad de montaje (`CAP_FS_MOUNT`), que tiene un
administrador. El núcleo lo comprueba y anota cada decisión en el
registro de auditoría.

## OPTIONS

- `-h, -?, --help` — mostrar la ayuda breve propia de esta orden.
- `--version` — mostrar el nombre y la versión de esta orden.

## EXAMPLES

- `label usb1` — mostrar el título del volumen montado como `usb1`.
- `label usb1 HOLIDAY` — darle el título `HOLIDAY`.
- `label /Storage/backup 'Backup 2026'` — dar título al volumen montado
  en `/Storage/backup`.
- `label usb1 ''` — borrar su título.

## EXIT STATUS

- `0` — el título se mostró o se estableció (o se escribió la ayuda o
  la versión).
- `1` — no se entendió la línea de órdenes, el nombre no designaba
  ningún volumen, el título no lo era o el volumen lo rechazó.
- `2` — falló el servicio de información del sistema, o la escritura en
  el terminal.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve (una
  etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `mount`
- `unmount`
- `df`
