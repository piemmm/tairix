## NAME

mount — lister les systèmes de fichiers montés, ou monter une image disque

## SYNOPSIS

`mount`

`mount [option...] image [target]`

## DESCRIPTION

Sans opérande, liste les systèmes de fichiers montés, une ligne
`source on target type fstype (options)` par endroit où un volume
apparaît. `-t` ne liste alors que les types que nomme sa liste séparée
par des virgules ; une liste commençant par `no` liste tous les types
qu'elle ne nomme pas.

Avec un opérande `image`, monte le volume que contient ce fichier
d'image disque : un disque optique ISO 9660 ou UDF (`.iso`), une
disquette Acorn ADFS ou AmigaDOS (`.adf`, `.adl`, `.adm`, `.ads`), une
partition de disque dur AmigaDOS (`.hdf`) ou un volume FAT32. Ce que
renferme l'image se reconnaît à son contenu, jamais à son nom. Le
volume est monté pour vous seul, sur `/Storage/name`, et nommé d'après
l'étiquette du volume ou, à défaut, d'après le nom de fichier de
l'image ; personne d'autre ne peut l'ouvrir. Il est monté en
lecture-écriture si vous pouvez écrire dans l'image et que son format
peut être écrit, et en lecture seule sinon, ce dont `mount` avertit.
Les programmes d'un volume monté depuis une image ne s'exécutent
jamais.

Un opérande `target` de la forme `/Storage/name` monte le volume sous
ce nom. Tout autre opérande `target`, un répertoire vide vous
appartenant sous `/Users`, y fait aussi apparaître le volume.

Le volume reste monté jusqu'à ce que vous le détachiez avec `unmount`,
ou que votre session de connexion se termine. Une image qui contient
un disque partitionné, l'un des disques d'une grappe RAID ou un
système de fichiers dont les propriétaires enregistrés sont toujours
respectés (ext2, ext3, ext4, ARXFS) n'est pas montée, et `mount` en
donne la raison.

## OPTIONS

- `-r, --read-only` — monter le volume en lecture seule (comme
  `-o ro`).
- `-w, --rw, --read-write` — monter le volume en lecture-écriture, ou
  pas du tout : une image dans laquelle vous ne pouvez pas écrire, ou
  dont le système de fichiers ne peut pas être écrit, n'est pas montée.
  Le dernier de `-r`, `-w`, `-o ro` et `-o rw` l'emporte.
- `-t, --types <type>` — le système de fichiers que l'image doit
  contenir, tel que la liste des montages le nomme (`iso9660`, `udf`,
  `adfs`, `affs`, `fat32`), ou `auto` pour celui qu'elle contient,
  quel qu'il soit. Une image n'est pas montée si elle en contient un
  autre, ou si aucune image disque ne contient le type demandé.
- `-o, --options <list>` — une liste, séparée par des virgules, de
  `ro`, `rw` (en lecture-écriture lorsque c'est possible, la valeur
  par défaut), `noexec`, `loop` et `name=name`, le nom que doit
  prendre le volume. Une image est de toute façon montée en `noexec`
  et `loop`.
- `-v, --verbose` — indiquer où le volume a été monté.
- `-l, --show-labels` — accepté pour la liste, qui n'a pas d'étiquettes
  à ajouter.
- `-h, -?, --help` — afficher l'aide courte de cette commande.

## EXAMPLES

- `mount` — lister les systèmes de fichiers montés.
- `mount Workbench.adf` — monter une image de disquette Amiga, sur
  `/Storage/workbench` si c'est son étiquette.
- `mount -r disc.iso /Storage/disc` — monter une image de disque
  optique en lecture seule sous le nom `disc`.
- `mount -v -t adfs Elite.adl` — monter une image de disquette Acorn,
  en indiquant où.

## EXIT STATUS

- `0` — la liste a été écrite, ou le volume a été monté (ou l'aide
  courte a été écrite).
- `1` — la ligne de commande n'a pas été comprise.
- `2` — la liste des montages n'a pas pu être lue, ou l'écriture sur
  le terminal a échoué.
- `32` — l'image n'a pas été montée.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
