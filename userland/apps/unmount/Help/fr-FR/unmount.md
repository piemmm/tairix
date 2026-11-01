## NAME

unmount — détacher un volume monté

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Met hors service le volume monté sous `name` : le système de fichiers
et le périphérique sont vidés, le montage sous `/Storage` est retiré
et la racine durable `id::` du volume est révoquée. `name` est le nom
de catalogue du volume (`usb1`), un répertoire où il est monté
(`/Storage/usb1`, ou celui auquel `mount image répertoire` l'a lié),
ou le nom de fichier de l'image disque depuis laquelle il a été monté
(`Workbench.adf`), comparé aux listes des montages et des vues de l'API
d'informations système ; un nom de fichier que porte aussi l'image d'un
autre utilisateur désigne la vôtre. Un nom de fichier qui pourrait
désigner plus d'un volume, ou dont la liste ne garde que le début, n'en
désigne aucun : nommez alors le volume par l'endroit où il est
monté.
Un chemin peut être relatif ou finir par une barre oblique : il est
résolu comme `mount` en résout un. Le volume privé d'un autre
utilisateur ne montre ni son nom de fichier ni les répertoires auxquels
il est lié : il se nomme donc par l'endroit où il est monté sous
`/Storage`.

Un volume dont le périphérique a été retiré alors qu'il portait
encore des écritures non validées reste visible comme
`unavailable-dirty` dans la liste des montages, et un `unmount` simple
refuse : les données retenues sont conservées pour une réinsertion
vérifiée. Ce refus vaut également pour un volume réinséré dont on n'a
pas pu établir qu'il était inchangé (`recovery-conflict`), qui conserve
encore ces données, et pour un volume dont les écritures non validées
n'ont pas pu être gardées (`unavailable-lost`), afin que leur perte
soit reconnue plutôt que passée sous silence. Un `unmount` simple est
aussi refusé pour un volume sur lequel des fichiers sont encore
ouverts, ou qu'un programme a pour répertoire de travail. `--force`
est la sortie délibérée — les fichiers encore ouverts sur le volume en
sont coupés, un programme qui y travaille reste dans un répertoire qui
n'existe plus, les données retenues sont abandonnées, le volume est
retiré et ce qui a été sacrifié est consigné dans le journal d'audit.
Sur un volume sain, `--force` vide et détache toujours proprement ;
rien n'est abandonné quand une validation propre est possible.

Vous pouvez détacher une image disque ou un partage réseau que vous
avez monté vous-même ; tout autre volume exige l'autorité de montage
(`CAP_FS_MOUNT`). Le noyau la vérifie et audite chaque décision. Les
volumes de démarrage permanents et les liaisons de vue du système ne
se détachent pas.

## OPTIONS

- `-f, --force` — démontage forcé : retirer le volume même quand des
  fichiers y sont ouverts, qu'un programme y travaille ou que ses
  données non validées ne peuvent pas être validées, en coupant du
  volume les fichiers ouverts et en abandonnant les données retenues.
- `-h, -?, --help` — afficher l'aide courte de cette commande.

## EXAMPLES

- `unmount usb1` — détacher proprement le volume monté comme `usb1`.
- `unmount /Storage/usb1` — la même chose, nommé par son point de
  montage.
- `unmount Workbench.adf` — détacher l'image disque montée depuis
  `Workbench.adf`.
- `unmount --force usb1` — retirer un volume indisponible en
  abandonnant ses données retenues non validées.

## EXIT STATUS

- `0` — le volume a été détaché (ou l'aide courte a été écrite).
- `1` — la ligne de commande n'a pas été comprise, ou l'appelant ne peut
  pas détacher le volume.
- `2` — le service d'informations système, ou l'écriture sur le
  terminal, a échoué.
- `32` — le volume n'était pas monté ou n'est pas détachable, le chemin
  qui le nomme n'a pu être résolu, ou son détachement a échoué.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `mount`
- `df`
- `man`
