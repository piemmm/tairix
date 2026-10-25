## NAME

mkdir — créer des répertoires

## SYNOPSIS

`mkdir [-pv] [-m MODE] [--] répertoire...`

## DESCRIPTION

Crée chaque répertoire donné en opérande, dans l'ordre. Sans `-p`, le
parent de chaque opérande doit déjà exister et l'opérande lui-même ne
doit pas exister ; un opérande impossible à créer est signalé et le
suivant est tout de même créé.

Avec `-p`, chaque ancêtre manquant est créé d'abord, du plus externe au
plus interne, et un opérande (ou un ancêtre) qui existe déjà comme
répertoire n'est pas une erreur. Un ancêtre qui existe comme fichier
échoue toujours : rien n'est jamais remplacé silencieusement.

Un nouveau répertoire prend le mode `a=rwx` moins l'umask. Avec `-m`,
chaque répertoire donné en opérande qui est créé reçoit exactement
`MODE` à la place, quel que soit l'umask : une valeur octale ou des
clauses symboliques, telles que `chmod` les lit, en partant de
`a=rwx`. Un ancêtre manquant que `-p` crée garde les droits
d'écriture et de recherche de son propriétaire, quelle que soit la
valeur de l'umask, si bien que le répertoire situé en dessous peut
toujours être créé. Un répertoire créé dans un répertoire
set-group-ID garde le bit dont il hérite, sauf si `MODE` le nomme. Un
opérande qui existe déjà garde son mode. `--` termine l'analyse des
options : chaque argument suivant est un chemin.

## OPTIONS

- `-m, --mode=MODE` — donner à chaque répertoire créé en opérande le
  mode `MODE`, octal ou symbolique comme pour `chmod`, plutôt que
  `a=rwx` moins l'umask.
- `-p, --parents` — créer les répertoires parents manquants ; un
  opérande qui est déjà un répertoire n'est pas une erreur.
- `-v, --verbose` — signaler chaque répertoire créé par
  `mkdir: created directory 'rép'`.
- `-h, -?` — afficher l'aide courte de cette commande (aussi `--help`).

## EXAMPLES

- `mkdir Notes` — créer un répertoire dans le répertoire courant.
- `mkdir -p Projects/os/build` — créer toute la chaîne, en sautant les
  parties qui existent déjà.
- `mkdir -pv Home:/tools/bin` — créer sous une racine d'alias, en
  signalant chaque nouveau répertoire.
- `mkdir -m 700 Private` — créer un répertoire que seul son
  propriétaire peut utiliser.

## EXIT STATUS

- `0` — chaque répertoire a été créé (ou, avec `-p`, existait déjà).
- `1` — un échec du système de fichiers ou de la sortie, ou un mode qui
  n'a pas pu être appliqué ; la raison est affichée sur la sortie
  d'erreur. Une ligne de commande non comprise, ou un `MODE` qui n'est
  pas un mode, sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

chmod, rmdir, rm, ls
