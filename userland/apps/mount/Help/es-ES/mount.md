## NAME

mount — listar los sistemas de archivos montados o montar una imagen de disco

## SYNOPSIS

`mount`

`mount [option...] image [target]`

## DESCRIPTION

Sin operandos, lista los sistemas de archivos montados, una línea
`source on target type fstype (options)` por cada lugar en el que se
muestra un volumen. `-t` lista entonces solo los tipos que nombra su
lista separada por comas; una lista que empieza por `no` lista todos
los tipos que no nombra.

Con un operando `image`, monta el volumen que contiene ese archivo de
imagen de disco: un disco ISO 9660 o UDF (`.iso`), un disquete Acorn
ADFS o AmigaDOS (`.adf`, `.adl`, `.adm`, `.ads`), una partición de
disco duro AmigaDOS (`.hdf`) o un volumen FAT32. Lo que guarda la
imagen se reconoce por su contenido, nunca por su nombre. El volumen se
monta solo para usted, en `/Storage/name`, con un nombre tomado de la
etiqueta del volumen o, en su defecto, del nombre de archivo de la
imagen; nadie más puede abrirlo. Se monta en modo de lectura y
escritura cuando usted puede escribir en la imagen y su formato admite
escritura, y en modo de solo lectura en caso contrario, en cuyo caso
`mount` lo advierte. Nunca se ejecutan programas desde un volumen
montado a partir de una imagen.

Un `target` de la forma `/Storage/name` monta el volumen bajo ese
nombre. Cualquier otro `target`, un directorio vacío de su propiedad
bajo `/Users` o en un volumen de `/Storage`, muestra también el volumen
allí; un directorio en un volumen FAT32 o ADFS no puede mostrarlo.

El volumen sigue montado hasta que usted lo desmonte con `unmount` o
termine su sesión. Una imagen que contiene un disco particionado, uno
de los discos de un conjunto RAID o un sistema de archivos cuyos
propietarios registrados se respetan siempre (ext2, ext3, ext4, ARXFS)
no se monta, y `mount` indica el motivo.

## OPTIONS

- `-r, --read-only` — montar el volumen en modo de solo lectura (como
  `-o ro`).
- `-w, --rw, --read-write` — montar el volumen en modo de lectura y
  escritura o no montarlo: una imagen en la que usted no puede
  escribir, o cuyo sistema de archivos no admite escritura, no se
  monta. Un `-r` posterior anula esta opción; un `-o ro` o un `-o rw`
  posterior no la anula. De `-r`, `-w`, `-o ro` y `-o rw`, la última
  decide si el volumen queda en modo de solo lectura.
- `-t, --types <list>` — los sistemas de archivos, separados por comas,
  de los cuales la imagen debe contener uno, tal como los nombra la
  lista de montajes (`iso9660`, `udf`, `adfs`, `affs`, `fat32`), o
  `auto` para cualquiera que contenga; una lista que empieza por `no`
  nombra los que la imagen no debe contener. Lo que guarda la imagen se
  reconoce por su contenido, así que el orden no elige nada: un disco
  que contiene a la vez UDF e ISO 9660 se monta como UDF, salvo que su
  ISO 9660 conserve registros Rock Ridge o RISC OS. Si la imagen no
  contiene ninguno de esos sistemas de archivos o si la lista no nombra
  ningún tipo que una imagen de disco contenga, no se monta.
- `-o, --options <list>` — una lista separada por comas de `ro`, `rw`
  (lectura y escritura cuando sea posible, el valor predeterminado),
  `noexec`, `loop` y `name=name`, el nombre que ha de tomar el volumen.
  Una imagen se monta como `noexec` y `loop` en cualquier caso.
- `-v, --verbose` — indicar dónde se montó el volumen.
- `-l, --show-labels` — se acepta para el listado, que no tiene
  etiquetas que añadir.
- `-h, -?, --help` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `mount` — listar los sistemas de archivos montados.
- `mount Workbench.adf` — montar una imagen de disquete de Amiga, en
  `/Storage/workbench` si esa es su etiqueta.
- `mount -r disc.iso /Storage/disc` — montar una imagen de disco en
  modo de solo lectura como `disc`.
- `mount -v -t adfs Elite.adl` — montar una imagen de disquete de
  Acorn, indicando dónde.

## EXIT STATUS

- `0` — se escribió la lista o se montó el volumen (o se escribió la
  ayuda breve).
- `1` — no se entendió la línea de órdenes.
- `2` — no se pudo leer la lista de montajes o no se pudo escribir en
  el terminal.
- `32` — la imagen no se montó.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve
  (una etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
