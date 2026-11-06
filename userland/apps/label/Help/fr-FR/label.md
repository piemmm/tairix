## NAME

label — afficher ou changer le titre d'un volume

## SYNOPSIS

`label [option...] name [title]`

## DESCRIPTION

Affiche le titre du volume que désigne `name` ou, si `title` est donné,
le définit. Le titre d'un volume est l'étiquette que son système de
fichiers enregistre : le nom sous lequel il est affiché, et celui qu'il
reçoit sous `/Storage` lors de son prochain montage. `name` est le nom
de catalogue du volume (`usb1`), un répertoire où il est monté
(`/Storage/usb1`), le nom de fichier de l'image disque depuis laquelle
il a été monté (`Workbench.adf`) ou sa racine durable (`id::…`),
désignés comme pour `unmount`. Un `title` vide efface le titre là où le
format du volume le permet.

Tant que le volume reste monté, son nom sous `/Storage` ne change pas :
rien de ce qui le désigne là ne cesse de fonctionner, et le nouveau
titre le nomme à partir de son prochain montage. Son identité et celle
de ses fichiers restent les mêmes.

Chaque format écrit un titre à sa manière, et un titre qu'il ne peut
pas enregistrer est refusé plutôt que raccourci. Un volume FAT accepte
jusqu'à onze lettres majuscules, chiffres, espaces et la ponctuation
qu'admet un nom de fichier court, sans espace au début ni à la fin ; un
volume ext2, ext3 ou ext4 jusqu'à seize octets ; un volume ADFS jusqu'à
dix caractères ; un volume AmigaDOS de un à trente ; un volume UDF
jusqu'à 126 ; et un volume ARXFS jusqu'à 252 octets, sauf s'il est
chiffré. Un disque gravé avec un titre, une archive et un partage
réseau n'en acceptent pas de nouveau.

Une image disque que vous avez montée vous-même est à vous de la
renommer. Un disque amovible — une clé USB, une carte — est à vous de le
renommer pendant que vous utilisez l'ordinateur lui-même, si vous pouvez
modifier le contenu de son dossier racine ; tout autre volume exige
l'autorité de montage (`CAP_FS_MOUNT`), que détient un administrateur.
Le noyau le vérifie et consigne chaque décision dans le journal d'audit.

## OPTIONS

- `-h, -?, --help` — afficher l'aide courte propre à cette commande.
- `--version` — afficher le nom et la version de cette commande.

## EXAMPLES

- `label usb1` — afficher le titre du volume monté comme `usb1`.
- `label usb1 HOLIDAY` — lui donner le titre `HOLIDAY`.
- `label /Storage/backup 'Backup 2026'` — donner un titre au volume
  monté sur `/Storage/backup`.
- `label usb1 ''` — effacer son titre.

## EXIT STATUS

- `0` — le titre a été affiché ou défini (ou l'aide ou la version a été
  écrite).
- `1` — la ligne de commande n'a pas été comprise, le nom ne désignait
  aucun volume, le titre n'en était pas un, ou le volume l'a refusé.
- `2` — le service d'informations système, ou l'écriture vers le
  terminal, a échoué.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `mount`
- `unmount`
- `df`
