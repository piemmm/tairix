## NAME

mount — lister les systèmes de fichiers montés, ou monter une image disque ou une archive

## SYNOPSIS

`mount`

`mount [option...] image [target]`

`mount --list-held`

`mount --replay-held name`

`mount --discard-held name`

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
appartenant sous `/Users` ou sur un volume de `/Storage`, y fait aussi
apparaître le volume ; un répertoire situé sur un volume FAT32 ou ADFS
ne peut pas le faire apparaître.

Une archive se monte de la même façon, ses éléments devenant les
fichiers et dossiers du volume : une archive zip (`.zip`), une archive
tar, non compressée ou compressée avec gzip ou bzip2 (`.tar`, `.tar.gz`,
`.tgz`, `.tar.bz2`), un fichier unique compressé avec gzip ou bzip2
(`.gz`, `.bz2`), une archive Spark ou ArcFS de RISC OS (`.spk`, `.arc`)
ou une archive RAR (`.rar`). Les types de fichiers et les attributs
RISC OS accompagnent chaque élément. Les modifications sont enregistrées
par remplacement du fichier de l'archive, si bien qu'une archive n'est
montée en lecture-écriture que si vous pouvez écrire à la fois dans le
fichier et dans le dossier qui le contient. Une archive RAR est toujours
montée en lecture seule. Une archive découpée en plusieurs parties, ou
dont les noms sont chiffrés, n'est pas montée, et un élément chiffré ne
peut pas être lu.

Le volume reste monté jusqu'à ce que vous le détachiez avec `unmount`,
ou que votre session de connexion se termine. Une image qui contient
un disque partitionné, l'un des disques d'une grappe RAID ou un
système de fichiers dont les propriétaires enregistrés sont toujours
respectés (ext2, ext3, ext4, ARXFS) n'est pas montée, et `mount` en
donne la raison.

Un disque remis en place alors qu'il retenait encore des modifications
qui n'y étaient pas encore écrites est monté en lecture seule, les
modifications conservées, quand son périphérique ne peut pas montrer
qu'il s'agit du même disque et non d'une copie ; la liste le marque
`[replay-unconfirmed]`, et `--list-held` ne liste que ces volumes.
`--replay-held` y écrit les modifications, si son contenu le montre
inchangé depuis ; `--discard-held` les abandonne et prend le disque tel
qu'il est. Dans les deux cas, le volume est monté à nouveau, en
lecture-écriture là où c'est possible. Répondre exige l'autorité de
montage (`CAP_FS_MOUNT`) ; pour un disque amovible, le bureau pose
aussi la question à l'utilisateur devant l'ordinateur.

## OPTIONS

- `-r, --read-only` — monter le volume en lecture seule (comme
  `-o ro`).
- `-w, --rw, --read-write` — monter le volume en lecture-écriture, ou
  pas du tout : une image dans laquelle vous ne pouvez pas écrire, ou
  dont le système de fichiers ne peut pas être écrit, n'est pas montée.
  Un `-r` ultérieur annule cette option ; un `-o ro` ou un `-o rw`
  ultérieur ne l'annule pas. Le dernier de `-r`, `-w`, `-o ro` et
  `-o rw` détermine si le volume est en lecture seule.
- `-t, --types <list>` — les systèmes de fichiers, séparés par des
  virgules, dont l'image doit contenir l'un, tels que la liste des
  montages les nomme (`iso9660`, `udf`, `adfs`, `affs`, `fat32`, `zip`,
  `tar`, `tar.gz`, `tar.bz2`, `gzip`, `bzip2`, `spark`, `arcfs`, `rar`),
  ou `auto` pour celui qu'elle contient, quel qu'il soit ; une liste
  commençant par `no` nomme ceux que l'image ne doit pas contenir. Ce
  que renferme l'image se reconnaît à son contenu, si bien que l'ordre
  ne choisit rien : un disque optique contenant à la fois UDF et
  ISO 9660 est monté en UDF, sauf si son ISO 9660 conserve des
  enregistrements Rock Ridge ou RISC OS. Une image n'est pas montée si
  elle ne contient aucun de ces systèmes de fichiers, ou si la liste ne
  nomme aucun type que contienne une image disque.
- `-o, --options <list>` — une liste, séparée par des virgules, de
  `ro`, `rw` (en lecture-écriture lorsque c'est possible, la valeur
  par défaut), `noexec`, `loop` et `name=name`, le nom que doit
  prendre le volume. Une image est de toute façon montée en `noexec`
  et `loop`.
- `-v, --verbose` — indiquer où le volume a été monté.
- `-l, --show-labels` — accepté pour la liste, qui n'a pas d'étiquettes
  à ajouter.
- `--list-held` — ne lister que les volumes dont les modifications
  attendent `--replay-held` ou `--discard-held`.
- `--replay-held <name>` — écrire sur son disque les modifications
  qu'un volume retient, si son contenu le montre encore inchangé ;
  `name` est le nom du volume dans `/Storage`, un répertoire où il est
  présenté, ou sa racine `id::`.
- `--discard-held <name>` — abandonner les modifications qu'un volume
  retient, en prenant son disque tel qu'il est.
- `-h, -?, --help` — afficher l'aide courte de cette commande.

## EXAMPLES

- `mount` — lister les systèmes de fichiers montés.
- `mount Workbench.adf` — monter une image de disquette Amiga, sur
  `/Storage/workbench` si c'est son étiquette.
- `mount -r disc.iso /Storage/disc` — monter une image de disque
  optique en lecture seule sous le nom `disc`.
- `mount -v -t adfs Elite.adl` — monter une image de disquette Acorn,
  en indiquant où.
- `mount Photos.zip` — monter une archive zip, en lecture-écriture si
  vous pouvez écrire dans l'archive et dans son dossier.
- `mount -r backup.tar.gz /Storage/backup` — monter une archive tar
  compressée en lecture seule sous le nom `backup`.
- `mount --list-held` — lister les volumes dont les modifications
  attendent une réponse.
- `mount --replay-held backup` — écrire sur son disque les
  modifications que retient `/Storage/backup`.

## EXIT STATUS

- `0` — la liste a été écrite, le volume a été monté, ou ses
  modifications retenues ont été écrites ou abandonnées (ou l'aide
  courte a été écrite).
- `1` — la ligne de commande n'a pas été comprise, ou vous ne pouvez
  pas répondre pour les modifications du volume.
- `2` — la liste des montages n'a pas pu être lue, ou l'écriture sur
  le terminal a échoué.
- `32` — l'image n'a pas été montée, ou les modifications du volume
  n'ont été ni écrites ni abandonnées.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
